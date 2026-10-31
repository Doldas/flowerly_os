#!/usr/bin/env python3
"""Evidence records for the remote provider Arch/KVM acceptance (#263).

    not-run  --commit <sha> --clean yes|no --reason <text> --out <file>
    validate <file> [--phase preparation|execution]

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
"""
import argparse
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
    commits = {r.get("git_commit") for r in results if isinstance(r, dict)}
    if len(commits) > 1:
        problems.append("records name more than one git_commit")
    if all(isinstance(r, dict) and r.get("test_status") in STATUSES for r in results):
        if doc.get("summary") != summarize(doc.get("phase"), results):
            problems.append("summary does not match the records")
    found = leaks(json.dumps(doc))
    if found:
        problems.append(f"possible secret or content leak: {', '.join(found)}")
    return problems


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
    args = parser.parse_args(argv[1:])
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
