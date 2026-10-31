#!/usr/bin/env python3
"""Evidence records for the remote provider Arch/KVM acceptance (#263).

    not-run  --commit <sha> --clean yes|no --reason <text> --out <file>
    validate <file> [--phase preparation|execution]
    bind     --guest-evidence <file> --host-facts <file> --out <file>
    verify-bound <file>

`not-run` writes the only evidence a preparation command may produce: one
NOT_RUN record per check in testbed/remote-provider-kvm/checks.json. It
has no way to write anything else.

`validate` checks a document against evidence.schema.json's structure and
the rules a schema cannot express:
- exactly one record per catalogued check;
- PASS only in an `execution` document, with the exact commit, a clean
  tree, the verified image, kernel, systemd and binary/unit hashes, an
  observation and timestamps;
- NOT_RUN always with a reason and without an observation;
- the summary agrees with the records;
- nothing that looks like a key, bearer header, private key, sentinel
  prompt or dummy credential.

The guest runner builds its records with `record()` from this module, so
the rules are the same in both places. Exit status 0 = valid.

The guest can never PASS E1, E2 or E3 on its own: they depend on facts
only the host sees (QEMU's running command line, the host-side egress
canary, the image digest and signature). `bind` finalizes those three from
the host facts and wraps everything into one acceptance record.
`verify-bound` re-derives that finalization from the guest's original
records and the host facts, and checks that commit, run id, image, binary
and unit hashes and tree state agree everywhere. Only a record whose
`verify-bound` verdict is PASS is an acceptance candidate; a guest
evidence document alone, a preparation document, or a partial run never is.
"""
import argparse
import copy
import hashlib
import json
import pathlib
import re
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
CATALOG = ROOT / "testbed/remote-provider-kvm/checks.json"
SUITE = "remote-provider-arch-kvm"
STATUSES = ("PASS", "FAIL", "INCONCLUSIVE", "NOT_RUN")
SHA40 = re.compile(r"^[0-9a-f]{40}$")
SHA64 = re.compile(r"^[0-9a-f]{64}$")
UTC = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")
RUN_ID = re.compile(r"^[a-z0-9-]{8,64}$")
RECORD_FIELDS = (
    "schema_version", "test_suite", "test_id", "exercises", "git_commit", "git_tree_clean",
    "guest_image", "guest_image_sha256", "guest_kernel", "systemd_version", "providerd_sha256",
    "unit_sha256", "test_started_at_utc", "test_finished_at_utc", "test_status", "expected",
    "observed", "classification", "skip_reason", "artifacts")
# Synthetic markers the harness plants (sentinel prompt, dummy key) and
# shapes of real secrets. Matched case-insensitively on the serialized file.
LEAK_PATTERNS = {
    "bearer_header": re.compile(r"bearer\s+\S", re.I),
    "authorization_header": re.compile(r"authorization\s*:", re.I),
    "openai_style_key": re.compile(r"\bsk-[A-Za-z0-9_-]{8,}"),
    "private_key_block": re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
    "sentinel_prompt": re.compile(r"FLOWERLY-SENTINEL-PROMPT", re.I),
    "dummy_credential": re.compile(r"flowerly-kvm-dummy-key", re.I),
    "model_text_sentinel": re.compile(r"FLOWERLY-SENTINEL-REPLY", re.I),
}


def utc_now():
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def catalog():
    return json.loads(CATALOG.read_text())["checks"]


def record(check, run, status, expected=None, observed=None, classification=None,
           skip_reason=None, artifacts=(), started=None, finished=None):
    """One evidence record; `run` holds the run-level facts."""
    return {
        "schema_version": 1, "test_suite": SUITE, "test_id": check["test_id"],
        "exercises": check["exercises"], "git_commit": run["git_commit"],
        "git_tree_clean": run["git_tree_clean"], "guest_image": run.get("guest_image"),
        "guest_image_sha256": run.get("guest_image_sha256"), "guest_kernel": run.get("guest_kernel"),
        "systemd_version": run.get("systemd_version"), "providerd_sha256": run.get("providerd_sha256"),
        "unit_sha256": run.get("unit_sha256"), "test_started_at_utc": started,
        "test_finished_at_utc": finished, "test_status": status, "expected": expected,
        "observed": observed, "classification": classification, "skip_reason": skip_reason,
        "artifacts": list(artifacts)}


def summarize(phase, results):
    counts = {status: 0 for status in STATUSES}
    for result in results:
        counts[result["test_status"]] += 1
    if counts["FAIL"]:
        overall = "FAIL"
    elif counts["INCONCLUSIVE"]:
        overall = "INCONCLUSIVE"
    elif counts["NOT_RUN"] or phase != "execution" or not results:
        overall = "NOT_RUN"
    else:
        overall = "PASS"
    return {"overall": overall, "counts": counts}


def document(phase, run_id, results):
    return {"schema_version": 1, "test_suite": SUITE, "phase": phase, "run_id": run_id,
            "summary": summarize(phase, results), "results": results}


def not_run(commit, clean, reason, run_id=None):
    run = {"git_commit": commit, "git_tree_clean": clean}
    run_id = run_id or f"prep-{commit[:12]}"
    results = [record(check, run, "NOT_RUN", skip_reason=reason) for check in catalog()]
    return document("preparation", run_id, results)


def leaks(text):
    return sorted(name for name, pattern in LEAK_PATTERNS.items() if pattern.search(text))


def validate(doc, phase=None):
    """Every problem found, as text; an empty list means valid."""
    problems = []
    if not isinstance(doc, dict):
        return ["document is not an object"]
    expected_keys = {"schema_version", "test_suite", "phase", "run_id", "summary", "results"}
    if set(doc) != expected_keys:
        problems.append(f"document keys {sorted(doc)} != {sorted(expected_keys)}")
    if doc.get("schema_version") != 1 or doc.get("test_suite") != SUITE:
        problems.append("wrong schema_version or test_suite")
    if doc.get("phase") not in ("preparation", "execution"):
        problems.append("phase must be preparation or execution")
    if phase and doc.get("phase") != phase:
        problems.append(f"phase is {doc.get('phase')!r}, expected {phase!r}")
    if not isinstance(doc.get("run_id"), str) or not RUN_ID.match(doc["run_id"]):
        problems.append("run_id is malformed")
    results = doc.get("results")
    if not isinstance(results, list):
        return problems + ["results is not a list"]
    by_check = {check["test_id"]: check for check in catalog()}
    seen = {}
    for index, result in enumerate(results):
        where = f"results[{index}]"
        if not isinstance(result, dict) or set(result) != set(RECORD_FIELDS):
            problems.append(f"{where} does not have exactly the record fields")
            continue
        bad_types = [key for key, kinds in (("test_id", str), ("test_status", str), ("exercises", str),
                                            ("classification", (str, type(None))),
                                            ("skip_reason", (str, type(None))))
                     if not isinstance(result[key], kinds)]
        if bad_types:
            problems.append(f"{where} has wrongly typed {', '.join(bad_types)}")
            continue
        test_id = result["test_id"]
        where = f"{where} ({test_id})"
        seen[test_id] = seen.get(test_id, 0) + 1
        check = by_check.get(test_id)
        if check is None:
            problems.append(f"{where} is not in the check catalog")
        elif result["exercises"] != check["exercises"]:
            problems.append(f"{where} exercises {result['exercises']!r}, catalog says {check['exercises']!r}")
        if result["schema_version"] != 1 or result["test_suite"] != SUITE:
            problems.append(f"{where} wrong schema_version or test_suite")
        if not isinstance(result["git_commit"], str) or not SHA40.match(result["git_commit"]):
            problems.append(f"{where} git_commit is not a full SHA")
        if not isinstance(result["git_tree_clean"], bool):
            problems.append(f"{where} git_tree_clean is not boolean")
        for key in ("guest_image_sha256", "providerd_sha256", "unit_sha256"):
            if result[key] is not None and not (isinstance(result[key], str) and SHA64.match(result[key])):
                problems.append(f"{where} {key} is not a SHA-256")
        for key in ("test_started_at_utc", "test_finished_at_utc"):
            if result[key] is not None and not (isinstance(result[key], str) and UTC.match(result[key])):
                problems.append(f"{where} {key} is not UTC")
        if not isinstance(result["artifacts"], list) or \
                not all(isinstance(a, str) and len(a) <= 200 for a in result["artifacts"]):
            problems.append(f"{where} artifacts must be a list of short strings")
        status = result["test_status"]
        if status not in STATUSES:
            problems.append(f"{where} test_status {status!r} is not allowed")
            continue
        if status == "NOT_RUN":
            if not result["skip_reason"]:
                problems.append(f"{where} NOT_RUN without skip_reason")
            if result["observed"] is not None:
                problems.append(f"{where} NOT_RUN with an observation")
        elif doc.get("phase") != "execution":
            problems.append(f"{where} {status} in a {doc.get('phase')} document: only an executed run observes")
        if status == "PASS":
            missing = [key for key in ("guest_image", "guest_image_sha256", "guest_kernel", "systemd_version",
                                       "providerd_sha256", "unit_sha256", "test_started_at_utc",
                                       "test_finished_at_utc", "observed", "expected")
                       if result[key] in (None, "")]
            if missing:
                problems.append(f"{where} PASS without {', '.join(missing)}")
            if result["git_tree_clean"] is not True:
                problems.append(f"{where} PASS from a dirty tree")
            if result["skip_reason"]:
                problems.append(f"{where} PASS with a skip_reason")
    for test_id in by_check:
        if seen.get(test_id, 0) != 1:
            problems.append(f"check {test_id} has {seen.get(test_id, 0)} records, needs exactly 1")
    commits = {str(r.get("git_commit")) for r in results if isinstance(r, dict)}
    if len(commits) > 1:
        problems.append("records name more than one git_commit")
    if all(isinstance(r, dict) and isinstance(r.get("test_status"), str) and r["test_status"] in STATUSES
           for r in results):
        if doc.get("summary") != summarize(doc.get("phase"), results):
            problems.append("summary does not match the records")
    found = leaks(json.dumps(doc))
    if found:
        problems.append(f"possible secret or content leak: {', '.join(found)}")
    return problems


# --- host finalization and binding -----------------------------------------

AWAITING_HOST = "awaiting_host"
HOST_CHECKS = ("E1", "E2", "E3")
SSH_FORWARD = re.compile(r"^hostfwd=tcp:127\.0\.0\.1:\d{1,5}-:22$")


def qemu_accel_kvm(cmdline):
    """Exactly one -accel, and it is kvm; no TCG anywhere."""
    accels = [cmdline[i + 1] for i, arg in enumerate(cmdline[:-1]) if arg == "-accel"]
    return accels == ["kvm"] and not any("tcg" in arg.lower() for arg in cmdline)


def qemu_network_restricted(cmdline):
    """Exactly one NIC: user networking, restrict=on, only the SSH forward
    on 127.0.0.1, no guestfwd; no other network option at all."""
    network_flags = {"-nic", "-netdev", "-net", "-device"}
    nics = [cmdline[i + 1] for i, arg in enumerate(cmdline[:-1]) if arg in network_flags]
    if len(nics) != 1 or "-nic" not in cmdline:
        return False
    options = nics[0].split(",")
    forwards = [o for o in options if o.startswith(("hostfwd=", "guestfwd="))]
    return (options[0] == "user" and "restrict=on" in options and len(forwards) == 1
            and bool(SSH_FORWARD.match(forwards[0])))


def finalize(guest_doc, host):
    """The guest's document with E1-E3 decided from the host facts.
    Deterministic: `verify-bound` recomputes it and compares."""
    doc = copy.deepcopy(guest_doc)
    when = host.get("finished_at_utc")
    qemu = (host.get("qemu") or {}).get("cmdline") or []
    image = host.get("image") or {}
    canary = host.get("egress_canary") or {}
    for record in doc["results"]:
        test_id = record["test_id"]
        if test_id not in HOST_CHECKS:
            continue
        guest_status, guest_class = record["test_status"], record["classification"]
        if guest_status == "PASS":
            status, cls = "INCONCLUSIVE", "guest_claimed_a_host_check"
        elif guest_status == "FAIL":
            status, cls = "FAIL", guest_class
        elif guest_class != AWAITING_HOST:
            status, cls = guest_status, guest_class
        elif test_id == "E1":
            status, cls = ("PASS", "kvm_verified") if qemu_accel_kvm(qemu) else (
                ("INCONCLUSIVE", "qemu_cmdline_missing") if not qemu else ("FAIL", "qemu_not_kvm"))
        elif test_id == "E2":
            foreign = canary.get("total_connections", 0) - canary.get("control_connections", 0)
            if not qemu:
                status, cls = "INCONCLUSIVE", "qemu_cmdline_missing"
            elif foreign != 0:
                status, cls = "FAIL", "host_canary_reached"
            elif not qemu_network_restricted(qemu):
                status, cls = "FAIL", "qemu_network_not_restricted"
            elif not (canary.get("live_before") and canary.get("live_after")):
                status, cls = "INCONCLUSIVE", "canary_not_proven_live"
            else:
                status, cls = "PASS", "restrict_verified_canary_untouched"
        else:  # E3
            verified = (image.get("sha256_actual") == image.get("sha256_expected") == image.get("lock_sha256")
                        and image.get("sha256_expected") is not None
                        and image.get("signature_verified") is True
                        and image.get("keyring_fingerprints") == [image.get("signing_key_fingerprint")]
                        and image.get("signing_key_fingerprint") == image.get("lock_fingerprint"))
            status, cls = ("PASS", "digest_and_signature_verified") if verified else ("FAIL", "image_unverified")
        record["observed"] = {"guest": record["observed"], "host": {
            "E1": {"qemu_accel": [qemu[i + 1] for i, a in enumerate(qemu[:-1]) if a == "-accel"]},
            "E2": {"qemu_network": [qemu[i + 1] for i, a in enumerate(qemu[:-1]) if a == "-nic"], "canary": canary},
            "E3": {k: image.get(k) for k in ("sha256_actual", "signature_verified", "signing_key_fingerprint")},
        }[test_id]}
        record.update(test_status=status, classification=cls, test_finished_at_utc=when,
                      test_started_at_utc=record["test_started_at_utc"] or when)
        if status != "NOT_RUN":
            record["skip_reason"] = None
    doc["summary"] = summarize(doc["phase"], doc["results"])
    return doc


def canonical_sha(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def bind(guest_doc, host):
    """Never raises on malformed input: a guest document or host facts that
    cannot be finalized are bound as they are and verify as INVALID."""
    facts = host if isinstance(host, dict) else {}
    try:
        final = finalize(guest_doc, host) if isinstance(guest_doc, dict) and isinstance(host, dict) \
            and not validate(guest_doc, "execution") else None
    except (KeyError, TypeError, AttributeError, IndexError, ValueError):
        final = None
    bound = {"schema_version": 1, "kind": "remote-provider-kvm-acceptance", "run_id": facts.get("run_id"),
             "approved_commit": facts.get("approved_commit"), "host": host, "guest_evidence": guest_doc,
             "guest_evidence_sha256": canonical_sha(guest_doc), "evidence": final}
    verdict, problems = verify_bound(bound)
    bound.update(verdict=verdict, problems=problems)
    return bound


def verify_bound(bound):
    """(verdict, problems). Verdict is INVALID whenever anything disagrees."""
    if not isinstance(bound, dict):
        return "INVALID", ["acceptance record is not an object"]
    problems = []
    host = bound.get("host")
    if not isinstance(host, dict):
        return "INVALID", ["host facts are missing or malformed"]
    guest_doc = bound.get("guest_evidence")
    commit = bound.get("approved_commit")
    if bound.get("kind") != "remote-provider-kvm-acceptance" or bound.get("schema_version") != 1:
        problems.append("not an acceptance record")
    if not isinstance(guest_doc, dict) or guest_doc.get("phase") != "execution":
        return "INVALID", problems + ["guest evidence is missing or not from an executed run"]
    if canonical_sha(guest_doc) != bound.get("guest_evidence_sha256"):
        problems.append("guest evidence does not match its recorded digest")
    guest_problems = validate(guest_doc, "execution")
    if guest_problems:
        # Malformed guest evidence is never finalized, let alone passed.
        return "INVALID", sorted(set(problems + [f"guest evidence: {p}" for p in guest_problems]))
    if not isinstance(host, dict) or not isinstance(host.get("image") or {}, dict) or \
            not isinstance(host.get("qemu") or {}, dict) or not isinstance(host.get("egress_canary") or {}, dict) \
            or not isinstance(host.get("build") or {}, dict):
        return "INVALID", sorted(set(problems + ["host facts are malformed"]))
    try:
        final = finalize(guest_doc, host)
    except (KeyError, TypeError, AttributeError, IndexError, ValueError) as err:
        return "INVALID", sorted(set(problems + [f"host facts cannot be applied: {type(err).__name__}"]))
    if final != bound.get("evidence"):
        problems.append("finalized evidence differs from the finalization the host facts produce")
    problems += [f"evidence: {p}" for p in validate(final, "execution")]
    if not (isinstance(commit, str) and SHA40.match(commit)):
        problems.append("approved_commit is not a full SHA")
    if not (host.get("head_at_start") == host.get("head_at_end") == commit):
        problems.append("HEAD at start/end is not the approved commit")
    if host.get("tree_clean_at_start") is not True or host.get("tree_clean_at_end") is not True:
        problems.append("working tree was not clean for the whole run")
    if not (bound.get("run_id") == host.get("run_id") == guest_doc.get("run_id")):
        problems.append("run ids disagree")
    image = host.get("image") or {}
    build = host.get("build") or {}
    for record in final["results"]:
        if record["test_status"] == "NOT_RUN":
            continue
        where = record["test_id"]
        if record["git_commit"] != commit or record["git_tree_clean"] is not True:
            problems.append(f"{where}: commit or tree state differs from the approved run")
        if record["providerd_sha256"] != build.get("providerd_sha256"):
            problems.append(f"{where}: providerd hash differs from the host build")
        if record["unit_sha256"] != build.get("unit_sha256"):
            problems.append(f"{where}: unit hash differs from the approved commit's template")
        if record["guest_image_sha256"] != image.get("sha256_expected"):
            problems.append(f"{where}: image digest differs from the verified image")
    if problems:
        return "INVALID", sorted(set(problems))
    return final["summary"]["overall"], []


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    make = sub.add_parser("not-run")
    make.add_argument("--commit", required=True)
    make.add_argument("--clean", required=True, choices=("yes", "no"))
    make.add_argument("--reason", required=True)
    make.add_argument("--out", required=True)
    check = sub.add_parser("validate")
    check.add_argument("file")
    check.add_argument("--phase", choices=("preparation", "execution"))
    binder = sub.add_parser("bind")
    binder.add_argument("--guest-evidence", required=True)
    binder.add_argument("--host-facts", required=True)
    binder.add_argument("--out", required=True)
    verifier = sub.add_parser("verify-bound")
    verifier.add_argument("file")
    args = parser.parse_args(argv[1:])
    if args.command in ("bind", "verify-bound"):
        if args.command == "bind":
            bound = bind(json.loads(pathlib.Path(args.guest_evidence).read_text()),
                         json.loads(pathlib.Path(args.host_facts).read_text()))
            with open(args.out, "x") as out:
                out.write(json.dumps(bound, indent=2, sort_keys=True) + "\n")
        else:
            try:
                bound = json.loads(pathlib.Path(args.file).read_text())
                bound["verdict"], bound["problems"] = verify_bound(bound)
            except (OSError, ValueError, KeyError, TypeError, AttributeError) as err:
                bound = {"verdict": "INVALID", "problems": [f"unreadable: {err}"]}
        print(json.dumps({"verdict": bound["verdict"], "problems": bound["problems"]}, indent=2))
        return 0 if bound["verdict"] == "PASS" else 1
    if args.command == "not-run":
        doc = not_run(args.commit, args.clean == "yes", args.reason)
        problems = validate(doc, "preparation")
        if problems:
            sys.exit("refusing to write invalid evidence:\n  " + "\n  ".join(problems))
        with open(args.out, "x") as out:
            out.write(json.dumps(doc, indent=2, sort_keys=True) + "\n")
        print(json.dumps({"evidence": args.out, "overall": doc["summary"]["overall"]}))
        return 0
    try:
        doc = json.loads(pathlib.Path(args.file).read_text())
    except (OSError, ValueError) as err:
        print(json.dumps({"valid": False, "problems": [f"unreadable: {err}"]}, indent=2))
        return 1
    problems = validate(doc, args.phase)
    print(json.dumps({"valid": not problems, "overall": (doc.get("summary") or {}).get("overall"),
                      "problems": problems}, indent=2))
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
