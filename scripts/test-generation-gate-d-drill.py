#!/usr/bin/env python3
"""Offline contract of the Gate D in-guest Admin drill (ADR-0023 G5)."""

import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
DRILL = ROOT / "testbed/generation-ab/g5-drill.py"
spec = importlib.util.spec_from_file_location("g5_drill", DRILL)
drill = importlib.util.module_from_spec(spec)
spec.loader.exec_module(drill)

GEN = "gen-0001"
IDENT = {
    "source_commit": "a" * 40,
    "rootfs_sha256": "b" * 64,
    "kernel_sha256": "c" * 64,
    "initramfs_sha256": "d" * 64,
    "input_manifest_sha256": "e" * 64,
    "build_id": "s7-b",
}


def register(**change):
    action = {
        "op": "register",
        "label": "B",
        "identity": dict(IDENT),
        "evidence_sha256": "f" * 64,
    }
    action.update(change)
    return action


def instruction(actions, slot="root-a", reboot=False, step=1):
    return {
        "schema": drill.SCHEMA,
        "step": step,
        "slot": slot,
        "actions": actions,
        "reboot": reboot,
    }


class FakeCtl:
    """flowerlyd's answers by subcommand; records every argv."""

    def __init__(self, **answers):
        self.calls = []
        self.answers = {
            "create": (0, {"id": GEN, "status": "CANDIDATE"}),
            "promote": (0, {"id": GEN}),
            "identity": (0, {"id": GEN}),
            "evidence": (0, {"id": GEN, "experiment": "exp-1"}),
            "test": (0, {"id": GEN, "status": "TESTED"}),
            "activate": (0, {"activation": {"id": 7}, "armed": {}}),
            "rollback": (0, {"activation": {"id": 8}, "armed": {}}),
            "inspect": (0, {"id": GEN}),
            "list": (0, {"generations": [], "count": 0}),
            "history": (0, {"activations": [], "count": 0}),
        }
        self.answers.update(answers)

    def __call__(self, argv):
        self.calls.append(argv)
        code, value = self.answers[argv[0]]
        if code == 0:
            return 0, json.dumps(value), ""
        return code, "", "Error: service error (Conflict): " + value


def decode(lines):
    parts = [
        line.split(" ", 3)[3]
        for line in lines
        if line.startswith("FLOWERLY_G5_DRILL step=")
    ]
    done = lines[-1]
    match = re.fullmatch(
        r"FLOWERLY_G5_DRILL_DONE step=(\d+) parts=(\d+) sha256=([0-9a-f]{64}) ok=(true|false)",
        done,
    )
    body = base64.b64decode("".join(parts))
    assert int(match.group(2)) == len(parts)
    assert hashlib.sha256(body).hexdigest() == match.group(3)
    return json.loads(body), match.group(4) == "true"


class InstructionTests(unittest.TestCase):
    def test_a_valid_instruction_is_accepted(self):
        doc = instruction(
            [
                register(),
                {
                    "op": "activate",
                    "generation": GEN,
                    "expect": "armed",
                    "acknowledges": 3,
                    "acknowledge_irreversible": True,
                },
                {"op": "rollback", "generation": GEN, "expect": "refused"},
                {"op": "inspect", "generation": GEN},
            ],
            reboot=True,
        )
        self.assertEqual(drill.load_instruction(json.dumps(doc))["step"], 1)

    def test_anything_unexpected_is_refused(self):
        bad_ident = dict(IDENT, rootfs_sha256="B" * 64)
        no_build = {k: v for k, v in IDENT.items() if k != "build_id"}
        cases = {
            "not json": "{",
            "schema": json.dumps(dict(instruction([]), schema="other/v1")),
            "extra key": json.dumps(dict(instruction([]), extra=1)),
            "slot": json.dumps(instruction([], slot="root-c")),
            "step": json.dumps(instruction([], step=0)),
            "bool step": json.dumps(instruction([], step=True)),
            "reboot": json.dumps(dict(instruction([]), reboot="yes")),
            "op": json.dumps(instruction([{"op": "revoke", "generation": GEN}])),
            "identity hex": json.dumps(instruction([register(identity=bad_ident)])),
            "identity keys": json.dumps(instruction([register(identity=no_build)])),
            "action key": json.dumps(instruction([register(force=True)])),
            "expect": json.dumps(
                instruction([{"op": "activate", "generation": GEN, "expect": "maybe"}])
            ),
            "no expect": json.dumps(
                instruction([{"op": "rollback", "generation": GEN}])
            ),
            "ack": json.dumps(
                instruction(
                    [
                        {
                            "op": "activate",
                            "generation": GEN,
                            "expect": "armed",
                            "acknowledges": "3",
                        }
                    ]
                )
            ),
            "rollback ack": json.dumps(
                instruction(
                    [
                        {
                            "op": "rollback",
                            "generation": GEN,
                            "expect": "armed",
                            "acknowledges": 3,
                        }
                    ]
                )
            ),
            "generation": json.dumps(
                instruction([{"op": "inspect", "generation": "../x"}])
            ),
            "too many": json.dumps(
                instruction([{"op": "inspect", "generation": GEN}] * 17)
            ),
        }
        for name, raw in cases.items():
            with self.subTest(name), self.assertRaises(ValueError):
                drill.load_instruction(raw)


class ActionTests(unittest.TestCase):
    def test_register_runs_the_admin_path_to_tested(self):
        ctl = FakeCtl()
        ok, entries = drill.run_actions(instruction([register()]), ctl)
        self.assertTrue(ok)
        self.assertEqual(
            [c[0] for c in ctl.calls],
            ["create", "promote", "identity", "evidence", "test"],
        )
        identity = ctl.calls[2]
        for key, value in IDENT.items():
            flag = "--" + key.replace("_", "-")
            self.assertEqual(identity[identity.index(flag) + 1], value)
        self.assertEqual(ctl.calls[4][ctl.calls[4].index("--experiment") + 1], "exp-1")
        self.assertEqual(
            entries[-1], {"call": "registered", "label": "B", "generation": GEN}
        )

    def test_activation_flags_reach_agenticctl(self):
        ctl = FakeCtl()
        action = {
            "op": "activate",
            "generation": GEN,
            "expect": "armed",
            "acknowledges": 3,
            "acknowledge_irreversible": True,
        }
        ok, _ = drill.run_actions(instruction([action], step=4), ctl)
        self.assertTrue(ok)
        argv = ctl.calls[0]
        self.assertEqual(argv[:2], ["activate", GEN])
        self.assertEqual(argv[argv.index("--acknowledges") + 1], "3")
        self.assertIn("--acknowledge-irreversible", argv)
        self.assertEqual(argv[argv.index("--reason") + 1], "G5 drill step 4")
        ok, _ = drill.run_actions(
            instruction([{"op": "rollback", "generation": GEN, "expect": "armed"}]), ctl
        )
        self.assertTrue(ok)
        self.assertNotIn("--acknowledges", ctl.calls[-1])
        self.assertNotIn("--acknowledge-irreversible", ctl.calls[-1])

    def test_an_unmet_expectation_stops_the_remaining_actions(self):
        later = {"op": "inspect", "generation": GEN}
        refused = FakeCtl(activate=(1, "needs an acknowledgement"))
        ok, entries = drill.run_actions(
            instruction(
                [{"op": "activate", "generation": GEN, "expect": "armed"}, later]
            ),
            refused,
        )
        self.assertFalse(ok)
        self.assertEqual(len(refused.calls), 1)
        self.assertIn("needs an acknowledgement", entries[0]["text"])

        armed = FakeCtl()
        ok, _ = drill.run_actions(
            instruction(
                [{"op": "rollback", "generation": GEN, "expect": "refused"}, later]
            ),
            armed,
        )
        self.assertFalse(ok, "an armed rollback met a refused expectation")
        self.assertEqual(len(armed.calls), 1)

        refused_ok = FakeCtl(rollback=(1, "cannot read"))
        ok, _ = drill.run_actions(
            instruction(
                [{"op": "rollback", "generation": GEN, "expect": "refused"}, later]
            ),
            refused_ok,
        )
        self.assertTrue(ok)
        self.assertEqual([c[0] for c in refused_ok.calls], ["rollback", "inspect"])

    def test_a_failed_registration_step_stops_registration(self):
        for step in ("create", "promote", "identity", "evidence", "test"):
            ctl = FakeCtl(**{step: (1, "refused")})
            with self.subTest(step):
                ok, _ = drill.run_actions(instruction([register()]), ctl)
                self.assertFalse(ok)
                self.assertEqual(ctl.calls[-1][0], step)
        not_tested = FakeCtl(test=(0, {"id": GEN, "status": "CANDIDATE"}))
        self.assertFalse(drill.run_actions(instruction([register()]), not_tested)[0])
        no_experiment = FakeCtl(evidence=(0, {"id": GEN}))
        self.assertFalse(drill.run_actions(instruction([register()]), no_experiment)[0])
        self.assertNotIn("test", [c[0] for c in no_experiment.calls])


class BootTests(unittest.TestCase):
    def run_drill(self, doc, slot="root-a", ctl=None, raw=None):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "g5-drill.json"
            running = Path(d) / "generation-root.json"
            running.write_text(json.dumps({"slot": slot}))
            if raw is not None or doc is not None:
                path.write_text(raw if raw is not None else json.dumps(doc))
            lines = []
            ctl = ctl or FakeCtl()
            due = drill.drill(path, running, ctl, lines.append)
            return due, lines, ctl

    def test_no_instruction_is_idle(self):
        due, lines, ctl = self.run_drill(None)
        self.assertEqual(
            (due, lines, ctl.calls), (False, ["FLOWERLY_G5_DRILL_IDLE"], [])
        )

    def test_another_slots_step_does_nothing(self):
        due, lines, ctl = self.run_drill(
            instruction([register()], slot="root-b", reboot=True)
        )
        self.assertFalse(due)
        self.assertEqual(ctl.calls, [])
        self.assertTrue(
            lines[0].startswith("FLOWERLY_G5_DRILL_SKIPPED step=1 running=root-a")
        )

    def test_a_refused_instruction_runs_nothing(self):
        due, lines, ctl = self.run_drill(None, raw='{"schema":"x"}')
        self.assertFalse(due)
        self.assertEqual(ctl.calls, [])
        self.assertTrue(lines[0].startswith("FLOWERLY_G5_DRILL_REFUSED"))

    def test_the_report_round_trips_and_always_carries_history(self):
        due, lines, ctl = self.run_drill(instruction([register()], reboot=True, step=2))
        self.assertTrue(due)
        report, ok = decode(lines)
        self.assertTrue(ok)
        self.assertEqual(
            (report["step"], report["slot"], report["ok"]), (2, "root-a", True)
        )
        self.assertEqual(
            [e["call"] for e in report["entries"]][-2:], ["list", "history"]
        )

        failing = FakeCtl(activate=(1, "refused"))
        due, lines, _ = self.run_drill(
            instruction(
                [{"op": "activate", "generation": GEN, "expect": "armed"}], reboot=True
            ),
            ctl=failing,
        )
        self.assertFalse(due, "a failed step must not reboot")
        report, ok = decode(lines)
        self.assertFalse(ok)
        self.assertEqual([c[0] for c in failing.calls], ["activate", "list", "history"])

    def test_large_reports_split_into_bounded_lines(self):
        report = {"ok": True, "entries": ["x" * 5000]}
        lines = drill.report_lines(3, report)
        self.assertGreater(len(lines), 3)
        self.assertTrue(all(len(line) < 600 for line in lines))
        self.assertEqual(decode(lines)[0], report)


if __name__ == "__main__":
    unittest.main(verbosity=1)
