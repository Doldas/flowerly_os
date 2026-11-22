#!/usr/bin/env python3
"""Archive Evolution Lab acceptance evidence (ADR-0024, #413).

``archive OUT --source-commit SHA40 --candidate DIR --evolutiond BIN
good=RUN bad=RUN rollback=RUN`` collects the three scenarios of one exact-SHA
``vm-test`` session into a new directory OUT for review. Each RUN is a
``FLOWERLY_LAB_OUT`` of ``scripts/vm-test.sh``; DIR is the candidate all three
ran from. It copies, per scenario, the recorded run's result directory and the
unrecorded initial boot's, each with its plan, ``result.json``,
``result.sha256`` and every serial log and probe report the result names, plus
the private flowerlyd's log, and the candidate's ``candidate.json``. Disks,
firmware variables and databases are never copied.

It refuses evidence that does not hold together:

- the candidate was not built at SHA40, or in the upstream lane (an upstream
  run is never acceptance evidence, ADR-0024 D4);
- a scenario is missing, repeated or unknown;
- a run's plans expect slots other than the candidate's (build id and
  identity hash), or a run is in another lane;
- a file differs from the hash its result records, or is too large;
- a recorded outcome is not the scenario's: ``good`` passed and recommends
  test; ``bad`` failed and recommends reject; ``rollback`` failed, recommends
  reject and its fallback to root-a booted healthy. Every initial boot passed;
- ``evolutiond lab verify`` refuses a copied result directory.

It then writes ``OUT/evidence.json`` (``flowerly-lab-evidence/v1``): the
commit, the lane, each scenario's outcome and result digests, and the SHA-256
of every other file in OUT.

``check OUT [--evolutiond BIN]`` re-reads an archive: the file list and every
hash in ``evidence.json``, the outcome rules, and, given BIN, ``lab verify``
of every result directory.

Standard library only.
"""

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys

SCHEMA = "flowerly-lab-evidence/v1"
CANDIDATE_SCHEMA = "flowerly-lab-candidate/v1"
RESULT_SCHEMA = "flowerly-lab-result/v1"
SCENARIOS = ("good", "bad", "rollback")
SLOTS = ("root-a", "root-b")
COMMIT = re.compile(r"[0-9a-f]{40}\Z")
SHA256 = re.compile(r"[0-9a-f]{64}\Z")
# Artifact names come from the result; only plain names in its own directory.
NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}\Z")
MAX_FILE = 1 << 20
MAX_JSON = 4 << 20
# (part in OUT/<scenario>, its directory in a vm-test output)
PARTS = (("initial", Path("run/initial/result")), ("result", Path("run/result")))
FIXED = ("plan.json", "result.json", "result.sha256")


class Refused(Exception):
    """The evidence does not hold together."""


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def read(path, what, limit=MAX_FILE):
    path = Path(path)
    if path.is_symlink() or not path.is_file():
        raise Refused(f"{what} {path} is missing or not a regular file")
    if path.stat().st_size > limit:
        raise Refused(f"{what} {path} is larger than {limit} bytes")
    return path.read_bytes()


def load_json(path, what):
    raw = read(path, what, MAX_JSON)
    try:
        return json.loads(raw)
    except ValueError as error:
        raise Refused(f"{what} {path} is not JSON: {error}") from None


def load_candidate(directory, source_commit):
    candidate = load_json(Path(directory) / "candidate.json", "candidate")
    if candidate.get("schema") != CANDIDATE_SCHEMA:
        raise Refused(f"candidate.json: schema must be {CANDIDATE_SCHEMA}")
    if candidate.get("source_commit") != source_commit:
        raise Refused(
            f"the candidate was built at {candidate.get('source_commit')}, "
            f"not {source_commit}"
        )
    if candidate.get("lane") != "reproducibility":
        raise Refused(
            "the candidate is not from the reproducibility lane; an upstream "
            "run is never acceptance evidence"
        )
    return candidate


def check_plan(plan, candidate, where):
    if plan.get("lane") != candidate["lane"]:
        raise Refused(f"{where}: the plan's lane is not the candidate's")
    # The initial plan boots and expects root-a only; every expected slot must
    # be the candidate's, and every booted slot expected.
    expect = plan.get("expect") or {}
    booted = {boot.get("slot") for boot in plan.get("boots", [])}
    if not booted or not booted <= set(expect) or not set(expect) <= set(SLOTS):
        raise Refused(f"{where}: the plan does not expect exactly slots it may boot")
    for slot, expected in expect.items():
        built = candidate["slots"][slot]
        for key in ("build_id", "identity_sha256"):
            if expected.get(key) != built[key]:
                raise Refused(f"{where}: the plan expects another {slot} ({key})")


def check_outcome(scenario, part, result, where):
    if result.get("schema") != RESULT_SCHEMA:
        raise Refused(f"{where}: result schema must be {RESULT_SCHEMA}")
    decision = result.get("decision", {})
    got = (decision.get("verdict"), decision.get("recommendation"))
    want = (
        ("passed", "test")
        if part == "initial" or scenario == "good"
        else (
            "failed",
            "reject",
        )
    )
    if got != want:
        raise Refused(
            f"{where}: recorded {got[0]}/{got[1]}, expected {want[0]}/{want[1]}"
        )
    fallback = decision.get("fallback")
    if part == "result" and scenario == "rollback":
        if not (fallback and fallback.get("booted") and fallback.get("healthy")):
            raise Refused(f"{where}: no healthy fallback to root-a was observed")
    elif fallback is not None:
        raise Refused(f"{where}: a fallback boot where none was planned")


def part_files(directory, result, where):
    """The files of one result directory, each checked against the result."""
    names = list(FIXED)
    for artifact in result.get("artifacts", []):
        name = artifact.get("name", "")
        if not NAME.fullmatch(name) or name in FIXED[1:]:
            raise Refused(f"{where}: artifact name {name!r} is not a plain file name")
        data = read(directory / name, f"{where} artifact")
        if (len(data), sha256_bytes(data)) != (
            artifact.get("bytes"),
            artifact.get("sha256"),
        ):
            raise Refused(f"{where}: {name} is not the file its result records")
        if name not in names:
            names.append(name)
    digest = read(directory / "result.sha256", where)
    if (
        digest
        != f"{sha256_bytes(read(directory / 'result.json', where, MAX_JSON))}  result.json\n".encode()
    ):
        raise Refused(f"{where}: result.sha256 does not match result.json")
    return names


def lab_verify(evolutiond, directory):
    """``lab verify`` exits 0 for an intact passing result, 3 for an intact
    failing one; either way its first line names the result's digest."""
    done = subprocess.run(
        [str(evolutiond), "lab", "verify", "--dir", str(directory)],
        capture_output=True,
        text=True,
        check=False,
    )
    digest = sha256_bytes(read(directory / "result.json", "result", MAX_JSON))
    verdict = load_json(directory / "result.json", "result")["decision"]["verdict"]
    lines = done.stdout.splitlines()
    if (
        done.returncode != {"passed": 0, "failed": 3}.get(verdict)
        or not lines
        or lines[0] != f"intact: evidence {digest}"
    ):
        detail = (done.stderr or done.stdout).strip().splitlines()[-1:] or [""]
        raise Refused(f"lab verify refused {directory}: {detail[0]}")


def scenario_runs(pairs):
    runs = {}
    for pair in pairs:
        scenario, sep, path = pair.partition("=")
        if not sep or scenario not in SCENARIOS or not path:
            raise Refused(
                f"{pair!r} is not SCENARIO=RUN with a scenario of {SCENARIOS}"
            )
        if scenario in runs:
            raise Refused(f"scenario {scenario} is given twice")
        runs[scenario] = Path(path)
    missing = [s for s in SCENARIOS if s not in runs]
    if missing:
        raise Refused(f"acceptance needs every scenario; missing {', '.join(missing)}")
    return runs


def summarize(scenario, out):
    entry = {}
    for part, _ in PARTS:
        directory = out / scenario / part
        result = load_json(directory / "result.json", f"{scenario} {part}")
        check_outcome(scenario, part, result, f"{scenario} {part}")
        entry[f"{part}_sha256"] = sha256_bytes(
            read(directory / "result.json", part, MAX_JSON)
        )
        if part == "result":
            decision = result["decision"]
            entry.update(
                verdict=decision["verdict"],
                recommendation=decision["recommendation"],
                fallback=decision["fallback"],
                candidate_generation=result["candidate"],
                disk_sha256=result["disk"]["sha256"],
            )
    return entry


def file_hashes(out):
    files = {}
    for path in sorted(out.rglob("*")):
        relative = path.relative_to(out).as_posix()
        if path.is_symlink() or not (path.is_dir() or path.is_file()):
            raise Refused(f"{relative} is not a regular file")
        if path.is_file() and relative != "evidence.json":
            files[relative] = sha256_bytes(read(path, "archived file", MAX_JSON))
    return files


def archive(out, source_commit, candidate_dir, evolutiond, pairs):
    if not COMMIT.fullmatch(source_commit):
        raise Refused("--source-commit must be a full 40-hex commit")
    out = Path(out)
    if out.exists() or out.is_symlink():
        raise Refused(f"{out} already exists")
    candidate = load_candidate(candidate_dir, source_commit)
    runs = scenario_runs(pairs)

    # Everything is checked before anything is written.
    copies = []
    for scenario in SCENARIOS:
        for part, relative in PARTS:
            where = f"{scenario} {part}"
            directory = runs[scenario] / relative
            check_plan(load_json(directory / "plan.json", where), candidate, where)
            result = load_json(directory / "result.json", where)
            if result.get("lane") != candidate["lane"]:
                raise Refused(f"{where}: the result's lane is not the candidate's")
            check_outcome(scenario, part, result, where)
            for name in part_files(directory, result, where):
                copies.append((directory / name, Path(scenario, part, name)))
        log = runs[scenario] / "flowerlyd/flowerlyd.log"
        read(log, f"{scenario} flowerlyd log")
        copies.append((log, Path(scenario, "flowerlyd.log")))

    out.mkdir(parents=True)
    try:
        return write_archive(
            out, source_commit, candidate, candidate_dir, evolutiond, copies
        )
    except BaseException:
        # OUT is ours: a partial archive must not look like one.
        shutil.rmtree(out)
        raise


def write_archive(out, source_commit, candidate, candidate_dir, evolutiond, copies):
    shutil.copyfile(Path(candidate_dir) / "candidate.json", out / "candidate.json")
    for source, target in copies:
        (out / target).parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, out / target)
    for scenario in SCENARIOS:
        for part, _ in PARTS:
            lab_verify(evolutiond, out / scenario / part)

    evidence = {
        "schema": SCHEMA,
        "source_commit": source_commit,
        "lane": candidate["lane"],
        "candidate": {
            "build_ids": {slot: candidate["slots"][slot]["build_id"] for slot in SLOTS},
            "input_manifest_sha256": candidate["input_manifest_sha256"],
        },
        "scenarios": {scenario: summarize(scenario, out) for scenario in SCENARIOS},
        "files": file_hashes(out),
    }
    raw = (json.dumps(evidence, indent=2, sort_keys=True) + "\n").encode()
    (out / "evidence.json").write_bytes(raw)
    return sha256_bytes(raw)


def check(out, evolutiond=None):
    out = Path(out)
    evidence = load_json(out / "evidence.json", "evidence")
    if evidence.get("schema") != SCHEMA:
        raise Refused(f"evidence.json: schema must be {SCHEMA}")
    if not COMMIT.fullmatch(str(evidence.get("source_commit"))):
        raise Refused("evidence.json: unverifiable source commit")
    candidate = load_candidate(out, evidence["source_commit"])
    files = file_hashes(out)
    if files != evidence.get("files"):
        changed = sorted(set(files.items()) ^ set(evidence.get("files", {}).items()))
        raise Refused(f"the archive differs from evidence.json: {changed[0][0]}")
    if set(evidence.get("scenarios", {})) != set(SCENARIOS):
        raise Refused("evidence.json must name every scenario")
    for scenario in SCENARIOS:
        for part, _ in PARTS:
            where = f"{scenario} {part}"
            directory = out / scenario / part
            check_plan(load_json(directory / "plan.json", where), candidate, where)
            part_files(directory, load_json(directory / "result.json", where), where)
            if evolutiond:
                lab_verify(evolutiond, directory)
        if summarize(scenario, out) != evidence["scenarios"][scenario]:
            raise Refused(f"evidence.json's summary of {scenario} is not its results'")
    return sha256_bytes(read(out / "evidence.json", "evidence", MAX_JSON))


def parser():
    top = argparse.ArgumentParser(
        prog="archive-lab-evidence.py", description=__doc__.split("\n")[0]
    )
    sub = top.add_subparsers(dest="command", required=True)
    a = sub.add_parser("archive", help="collect one session's three scenarios")
    a.add_argument("out")
    a.add_argument("runs", nargs="+", metavar="SCENARIO=RUN")
    a.add_argument("--source-commit", required=True)
    a.add_argument("--candidate", required=True)
    a.add_argument("--evolutiond", required=True)
    c = sub.add_parser("check", help="re-read an archive")
    c.add_argument("out")
    c.add_argument("--evolutiond")
    return top


def main(argv):
    options = parser().parse_args(argv)
    try:
        if options.command == "archive":
            digest = archive(
                options.out,
                options.source_commit,
                options.candidate,
                options.evolutiond,
                options.runs,
            )
        else:
            digest = check(options.out, options.evolutiond)
    except (Refused, OSError) as error:
        print(f"lab evidence: {error}", file=sys.stderr)
        return 1
    print(f"{digest}  {Path(options.out) / 'evidence.json'}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
