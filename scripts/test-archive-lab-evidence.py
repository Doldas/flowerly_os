#!/usr/bin/env python3
"""Offline contract of the Evolution Lab evidence archive (#413)."""

import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
TOOL = ROOT / "scripts/archive-lab-evidence.py"


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


evidence = load("archive_lab_evidence", TOOL)

COMMIT = "c" * 40
BUILT = {
    "root-a": {"build_id": "lab-a", "identity_sha256": "a" * 64},
    "root-b": {"build_id": "lab-b", "identity_sha256": "b" * 64},
}
PASSED = {
    "verdict": "passed",
    "recommendation": "test",
    "fallback": None,
    "reasons": [],
}
FAILED = {
    "verdict": "failed",
    "recommendation": "reject",
    "fallback": None,
    "reasons": ["x"],
}
FELL_BACK = dict(
    FAILED,
    fallback={
        "after": "candidate-1",
        "boot": "fallback",
        "booted": True,
        "healthy": True,
    },
)
RECORDED = {
    "good": ([("candidate", "root-b")], PASSED),
    "bad": ([("candidate", "root-b")], FAILED),
    "rollback": (
        [("candidate-1", "root-b"), ("candidate-2", "root-b"), ("fallback", "root-a")],
        FELL_BACK,
    ),
}

# Prints what `evolutiond lab verify --dir D` prints for an intact result and
# exits as it does: 0 passed, 3 failed. STUB_VERIFY_FAIL makes it refuse.
STUB = """#!{python}
import hashlib, json, os, sys
directory = sys.argv[sys.argv.index("--dir") + 1]
if os.environ.get("STUB_VERIFY_FAIL"):
    print("refused: boot candidate does not match its serial log", file=sys.stderr)
    sys.exit(1)
raw = open(os.path.join(directory, "result.json"), "rb").read()
print("intact: evidence " + hashlib.sha256(raw).hexdigest())
sys.exit(0 if json.loads(raw)["decision"]["verdict"] == "passed" else 3)
"""


def sha(data):
    return hashlib.sha256(data).hexdigest()


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def write_result(directory, lane, boots, decision, expect):
    plan = {
        "schema": "flowerly-lab-plan/v1",
        "lane": lane,
        "expect": {slot: dict(BUILT[slot]) for slot in expect},
        "boots": [
            {"label": label, "slot": slot, "subject": "candidate"}
            for label, slot in boots
        ],
    }
    write_json(directory / "plan.json", plan)
    files = [("plan", "plan.json")]
    for index, (label, _) in enumerate(boots, 1):
        (directory / f"{index}-{label}.serial.log").write_text(f"serial of {label}\n")
        (directory / f"{index}-{label}.report.json").write_text(
            f'{{"boot": "{label}"}}\n'
        )
        files += [
            ("serial-log", f"{index}-{label}.serial.log"),
            ("probe-report", f"{index}-{label}.report.json"),
        ]
    # Never archived: firmware variables are binary and per boot.
    (directory / "ovmf-vars.fd").write_bytes(b"\0" * 64)
    artifacts = []
    for kind, name in files:
        data = (directory / name).read_bytes()
        artifacts.append(
            {"bytes": len(data), "kind": kind, "name": name, "sha256": sha(data)}
        )
    result = {
        "schema": "flowerly-lab-result/v1",
        "lane": lane,
        "candidate": "gen-" + "1" * 32,
        "disk": {"sha256": "e" * 64},
        "decision": decision,
        "artifacts": artifacts,
    }
    rehash(directory, result)


def rehash(directory, result):
    write_json(directory / "result.json", result)
    digest = sha((directory / "result.json").read_bytes())
    (directory / "result.sha256").write_text(f"{digest}  result.json\n")


def write_run(base, scenario, lane="reproducibility"):
    run = base / scenario
    write_result(
        run / "run/initial/result", lane, [("initial", "root-a")], PASSED, ["root-a"]
    )
    boots, decision = RECORDED[scenario]
    write_result(run / "run/result", lane, boots, decision, ["root-a", "root-b"])
    (run / "run/candidate.img").write_bytes(b"\0" * 64)
    (run / "flowerlyd/state").mkdir(parents=True)
    (run / "flowerlyd/state/state.db").write_bytes(b"SQLite format 3\0")
    (run / "flowerlyd/flowerlyd.log").write_text("flowerlyd log\n")
    return run


def write_candidate(directory, lane="reproducibility", commit=COMMIT):
    write_json(
        directory / "candidate.json",
        {
            "schema": "flowerly-lab-candidate/v1",
            "source_commit": commit,
            "lane": lane,
            "input_manifest_sha256": "d" * 64,
            "slots": BUILT,
        },
    )


class Archive(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.base = Path(self.tmp.name)
        self.stub = self.base / "evolutiond"
        self.stub.write_text(STUB.format(python=sys.executable))
        self.stub.chmod(self.stub.stat().st_mode | stat.S_IXUSR)
        self.candidate = self.base / "candidate"
        write_candidate(self.candidate)
        self.runs = {s: write_run(self.base / "runs", s) for s in evidence.SCENARIOS}
        self.out = self.base / "out"
        os.environ.pop("STUB_VERIFY_FAIL", None)

    def tearDown(self):
        os.environ.pop("STUB_VERIFY_FAIL", None)
        self.tmp.cleanup()

    def pairs(self, runs=None):
        return [f"{s}={p}" for s, p in (runs or self.runs).items()]

    def archive(self, pairs=None, commit=COMMIT):
        return evidence.archive(
            self.out, commit, self.candidate, self.stub, pairs or self.pairs()
        )

    def refused(self, needle, **kwargs):
        with self.assertRaisesRegex(evidence.Refused, needle):
            self.archive(**kwargs)
        self.assertFalse(self.out.exists(), "a refused archive must leave nothing")

    def test_archive_collects_three_scenarios_and_check_rereads_them(self):
        digest = self.archive()
        doc = json.loads((self.out / "evidence.json").read_text())
        self.assertEqual(digest, sha((self.out / "evidence.json").read_bytes()))
        self.assertEqual(doc["schema"], "flowerly-lab-evidence/v1")
        self.assertEqual(doc["source_commit"], COMMIT)
        self.assertEqual(doc["lane"], "reproducibility")
        self.assertEqual(
            doc["candidate"]["build_ids"], {"root-a": "lab-a", "root-b": "lab-b"}
        )
        self.assertEqual(
            {
                s: (v["verdict"], v["recommendation"])
                for s, v in doc["scenarios"].items()
            },
            {
                "good": ("passed", "test"),
                "bad": ("failed", "reject"),
                "rollback": ("failed", "reject"),
            },
        )
        self.assertTrue(doc["scenarios"]["rollback"]["fallback"]["healthy"])
        rollback = self.runs["rollback"] / "run/result/result.json"
        self.assertEqual(
            doc["scenarios"]["rollback"]["result_sha256"], sha(rollback.read_bytes())
        )
        # Only text evidence: no disk, firmware variables or database.
        names = set(doc["files"])
        self.assertIn("candidate.json", names)
        self.assertIn("rollback/result/3-fallback.serial.log", names)
        self.assertIn("good/initial/1-initial.report.json", names)
        self.assertIn("bad/flowerlyd.log", names)
        self.assertFalse([n for n in names if n.endswith((".img", ".fd", ".db"))])
        self.assertEqual(
            names,
            {
                p.relative_to(self.out).as_posix()
                for p in self.out.rglob("*")
                if p.is_file()
            }
            - {"evidence.json"},
        )
        self.assertEqual(evidence.check(self.out, self.stub), digest)
        self.assertEqual(evidence.check(self.out), digest)

    def test_main_prints_the_digest_and_refuses_with_status_one(self):
        argv = [
            "archive",
            str(self.out),
            *self.pairs(),
            "--source-commit",
            COMMIT,
            "--candidate",
            str(self.candidate),
            "--evolutiond",
            str(self.stub),
        ]
        printed = io.StringIO()
        with contextlib.redirect_stdout(printed):
            self.assertEqual(evidence.main(argv), 0)
        self.assertRegex(printed.getvalue(), r"\A[0-9a-f]{64}  .*evidence\.json\n\Z")
        with contextlib.redirect_stderr(io.StringIO()) as error:
            self.assertEqual(evidence.main(argv), 1)
        self.assertIn("already exists", error.getvalue())

    def test_refuses_a_candidate_from_another_commit(self):
        self.refused("built at", commit="f" * 40)

    def test_refuses_a_short_commit(self):
        with self.assertRaisesRegex(evidence.Refused, "40-hex"):
            self.archive(commit="c" * 7)

    def test_refuses_the_upstream_lane(self):
        write_candidate(self.candidate, lane="upstream")
        self.refused("never acceptance evidence")

    def test_refuses_a_missing_repeated_or_unknown_scenario(self):
        pairs = self.pairs()
        self.refused("missing rollback", pairs=pairs[:2])
        self.refused("given twice", pairs=pairs + pairs[:1])
        self.refused("SCENARIO=RUN", pairs=pairs + [f"slow={self.base}"])

    def test_refuses_an_outcome_that_is_not_the_scenarios(self):
        directory = self.runs["good"] / "run/result"
        result = json.loads((directory / "result.json").read_text())
        result["decision"] = FAILED
        rehash(directory, result)
        self.refused("good result: recorded failed/reject, expected passed/test")

    def test_refuses_a_failed_initial_boot(self):
        directory = self.runs["bad"] / "run/initial/result"
        result = json.loads((directory / "result.json").read_text())
        result["decision"] = FAILED
        rehash(directory, result)
        self.refused("bad initial: recorded failed/reject")

    def test_refuses_a_rollback_without_a_healthy_fallback(self):
        directory = self.runs["rollback"] / "run/result"
        result = json.loads((directory / "result.json").read_text())
        result["decision"] = dict(
            FELL_BACK, fallback=dict(FELL_BACK["fallback"], healthy=False)
        )
        rehash(directory, result)
        self.refused("no healthy fallback")

    def test_refuses_an_unplanned_fallback(self):
        directory = self.runs["bad"] / "run/result"
        result = json.loads((directory / "result.json").read_text())
        result["decision"] = FELL_BACK
        rehash(directory, result)
        self.refused("a fallback boot where none was planned")

    def test_refuses_a_plan_for_other_slots(self):
        directory = self.runs["bad"] / "run/result"
        plan = json.loads((directory / "plan.json").read_text())
        plan["expect"]["root-b"]["build_id"] = "lab-x"
        write_json(directory / "plan.json", plan)
        self.refused(r"expects another root-b \(build_id\)")

    def test_refuses_a_plan_that_boots_an_unexpected_slot(self):
        directory = self.runs["good"] / "run/initial/result"
        plan = json.loads((directory / "plan.json").read_text())
        plan["boots"][0]["slot"] = "root-b"
        write_json(directory / "plan.json", plan)
        self.refused("exactly slots it may boot")

    def test_refuses_a_run_in_another_lane(self):
        directory = self.runs["good"] / "run/result"
        result = json.loads((directory / "result.json").read_text())
        result["lane"] = "upstream"
        rehash(directory, result)
        self.refused("result's lane")

    def test_refuses_an_edited_serial_log(self):
        log = self.runs["rollback"] / "run/result/2-candidate-2.serial.log"
        log.write_text("edited\n")
        self.refused("2-candidate-2.serial.log is not the file its result records")

    def test_refuses_a_stale_result_digest(self):
        (self.runs["good"] / "run/result/result.sha256").write_text(
            "0" * 64 + "  result.json\n"
        )
        self.refused("result.sha256 does not match")

    def test_refuses_an_artifact_outside_its_directory(self):
        directory = self.runs["good"] / "run/result"
        result = json.loads((directory / "result.json").read_text())
        result["artifacts"][1]["name"] = "../../flowerlyd/flowerlyd.log"
        rehash(directory, result)
        self.refused("not a plain file name")

    def test_refuses_an_oversized_file(self):
        (self.runs["good"] / "flowerlyd/flowerlyd.log").write_bytes(
            b"x" * (evidence.MAX_FILE + 1)
        )
        self.refused("larger than")

    def test_refuses_a_symlinked_file(self):
        log = self.runs["good"] / "flowerlyd/flowerlyd.log"
        log.unlink()
        log.symlink_to(self.base / "candidate/candidate.json")
        self.refused("not a regular file")

    def test_refuses_an_existing_out(self):
        self.out.mkdir()
        with self.assertRaisesRegex(evidence.Refused, "already exists"):
            self.archive()

    def test_a_copy_lab_verify_refuses_leaves_nothing(self):
        os.environ["STUB_VERIFY_FAIL"] = "1"
        self.refused("lab verify refused .*good/initial: refused: boot candidate")


class Check(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.base = Path(tmp.name)
        self.candidate = self.base / "candidate"
        write_candidate(self.candidate)
        runs = {s: write_run(self.base / "runs", s) for s in evidence.SCENARIOS}
        stub = self.base / "evolutiond"
        stub.write_text(STUB.format(python=sys.executable))
        stub.chmod(stub.stat().st_mode | stat.S_IXUSR)
        self.out = self.base / "out"
        evidence.archive(
            self.out,
            COMMIT,
            self.candidate,
            stub,
            [f"{s}={p}" for s, p in runs.items()],
        )

    def test_refuses_an_edited_file(self):
        with (self.out / "bad/flowerlyd.log").open("a") as log:
            log.write("x\n")
        with self.assertRaisesRegex(
            evidence.Refused, "differs from evidence.json: bad/flowerlyd.log"
        ):
            evidence.check(self.out)

    def test_refuses_an_added_file(self):
        (self.out / "good/result/extra.txt").write_text("x\n")
        with self.assertRaisesRegex(evidence.Refused, "good/result/extra.txt"):
            evidence.check(self.out)

    def test_refuses_a_removed_file(self):
        (self.out / "rollback/result/3-fallback.report.json").unlink()
        with self.assertRaisesRegex(evidence.Refused, "3-fallback.report.json"):
            evidence.check(self.out)

    def test_refuses_a_summary_its_results_do_not_support(self):
        path = self.out / "evidence.json"
        doc = json.loads(path.read_text())
        doc["scenarios"]["bad"]["verdict"] = "passed"
        write_json(path, doc)
        with self.assertRaisesRegex(evidence.Refused, "summary of bad"):
            evidence.check(self.out)


if __name__ == "__main__":
    unittest.main()
