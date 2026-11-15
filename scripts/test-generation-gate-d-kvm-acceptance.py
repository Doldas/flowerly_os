#!/usr/bin/env python3
"""Offline contract of the Gate D KVM acceptance harness (ADR-0023 G5).

The harness talks to the guest only through the drill's console report and
the ESP; these tests pin how it reads one and writes the other. They never
start QEMU.
"""

import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, ROOT / file)
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


gate = load("gate_d_kvm", "scripts/generation-gate-d-kvm-acceptance.py")
drill = load("g5_drill_for_host", "testbed/generation-ab/g5-drill.py")

PA = "11111111-1111-4111-8111-111111111111"
PB = "22222222-2222-4222-8222-222222222222"


def manifest():
    def root(name, uuid, digest, build):
        return {
            "name": name,
            "partuuid": uuid,
            "sha256": digest,
            "offset_bytes": 0,
            "size_bytes": 1,
            "filesystem": "erofs",
            "boot": {"kernel_sha256": "c" * 64, "initramfs_sha256": "d" * 64},
            # The slot's baked generation-root.json, verbatim.
            "identity": {
                "schema": "flowerly-generation-root/v2",
                "source_commit": "a" * 40,
                "input_manifest_sha256": "e" * 64,
                "build_id": build,
                "slot": name,
                "root_partuuid": uuid,
                "schema_version": 16,
                "min_reader": 16,
            },
        }

    return {
        "partitions": [
            {"name": "esp", "partuuid": "x", "offset_bytes": 0, "size_bytes": 1},
            root("root-a", PA, "1" * 64, "s7-a"),
            root("root-b", PB, "2" * 64, "s7-b"),
        ]
    }


def serial(report, noise="[  12.3] kernel: noise\r\n"):
    lines = drill.report_lines(report["step"], report)
    return noise + "".join(line + "\r\n" + noise for line in lines)


def report(step=3, ok=True, entries=None):
    return {
        "schema": drill.SCHEMA + "-report",
        "step": step,
        "slot": "root-b",
        "ok": ok,
        "entries": entries if entries is not None else [],
    }


def activation(id_, generation, state, kind="ACTIVATE", **extra):
    row = {
        "id": id_,
        "generation": generation,
        "kind": kind,
        "state": state,
        "irreversible": False,
        "backup_sha256": None,
        "acknowledges": None,
    }
    row.update(extra)
    return row


def readable(rows):
    return [
        {"call": "list", "code": 0, "reply": {"count": 0}, "text": ""},
        {
            "call": "history",
            "code": 0,
            "reply": {"count": len(rows), "activations": rows},
            "text": "",
        },
    ]


class ParseDrillTests(unittest.TestCase):
    def test_round_trip_through_console_noise(self):
        doc = report(entries=[{"call": "x", "text": "y" * 2000}])
        self.assertGreater(len(drill.report_lines(3, doc)), 2)
        self.assertEqual(gate.parse_drill(serial(doc), 3), doc)

    def test_absent_step_is_none(self):
        self.assertIsNone(gate.parse_drill(serial(report(step=2)), 3))

    def test_duplicated_identical_parts_are_accepted(self):
        doc = report()
        lines = drill.report_lines(3, doc)
        text = "\n".join([lines[0], *lines]) + "\n"
        self.assertEqual(gate.parse_drill(text, 3), doc)

    def test_refuses_missing_part(self):
        lines = drill.report_lines(3, report(entries=[{"t": "z" * 2000}]))
        text = "\n".join(lines[1:]) + "\n"
        with self.assertRaisesRegex(ValueError, "missing parts"):
            gate.parse_drill(text, 3)

    def test_refuses_digest_mismatch(self):
        text = re.sub(r"sha256=[0-9a-f]{64}", "sha256=" + "0" * 64, serial(report()))
        with self.assertRaisesRegex(ValueError, "digest"):
            gate.parse_drill(text, 3)

    def test_refuses_two_done_lines(self):
        text = serial(report()) * 2
        with self.assertRaisesRegex(ValueError, "more than once"):
            gate.parse_drill(text, 3)

    def test_refuses_conflicting_part_copies(self):
        lines = drill.report_lines(3, report())
        forged = base64.b64encode(b'{"forged":1}').decode()
        text = "\n".join([f"FLOWERLY_G5_DRILL step=3 part=1/1 {forged}", *lines])
        with self.assertRaisesRegex(ValueError, "differs"):
            gate.parse_drill(text, 3)

    def test_refuses_ok_flag_disagreement(self):
        body = json.dumps(report(ok=False)).encode()
        digest = hashlib.sha256(body).hexdigest()
        text = (
            "FLOWERLY_G5_DRILL step=3 part=1/1 "
            + base64.b64encode(body).decode()
            + f"\nFLOWERLY_G5_DRILL_DONE step=3 parts=1 sha256={digest} ok=true\n"
        )
        with self.assertRaisesRegex(ValueError, "disagrees"):
            gate.parse_drill(text, 3)


class InstructionTests(unittest.TestCase):
    def test_instruction_passes_the_guest_validator(self):
        doc = gate.instruction(
            2,
            "root-a",
            [{"op": "activate", "generation": "g1", "expect": "armed"}],
            True,
        )
        self.assertEqual(drill.load_instruction(json.dumps(doc)), doc)

    def test_instruction_refuses_what_the_guest_would_refuse(self):
        with self.assertRaises(ValueError):
            gate.instruction(1, "root-c", [], False)
        with self.assertRaises(ValueError):
            gate.instruction(
                1,
                "root-a",
                [{"op": "activate", "generation": "g", "expect": "maybe"}],
                False,
            )

    def test_registration_from_manifest(self):
        action = gate.registration(manifest(), "root-b", "B")
        drill.validate_action(action, 0)
        self.assertEqual(action["identity"]["rootfs_sha256"], "2" * 64)
        self.assertEqual(action["identity"]["build_id"], "s7-b")
        self.assertEqual(action["identity"]["kernel_sha256"], "c" * 64)
        expected = hashlib.sha256(
            json.dumps(action["identity"], sort_keys=True).encode()
        ).hexdigest()
        self.assertEqual(action["evidence_sha256"], expected)

    def test_registration_refuses_unstaged_slot(self):
        doc = manifest()
        del doc["partitions"][2]["boot"]
        with self.assertRaisesRegex(ValueError, "no staged boot payload"):
            gate.registration(doc, "root-b", "B")

    def test_registration_refuses_identity_of_another_slot(self):
        doc = manifest()
        doc["partitions"][2]["identity"]["root_partuuid"] = PA
        with self.assertRaisesRegex(ValueError, "another slot"):
            gate.registration(doc, "root-b", "B")


class EspEditTests(unittest.TestCase):
    ENTRY = (
        "title Flowerly OS slot A\nsort-key flowerly\nversion 4\n"
        "linux /flowerly/a/vmlinuz\noptions root=PARTUUID=x ro\n"
    )

    def test_with_option_appends_to_options_line(self):
        out = gate.with_option(self.ENTRY, gate.FAIL_HEALTH)
        self.assertIn("options root=PARTUUID=x ro " + gate.FAIL_HEALTH + "\n", out)
        self.assertIn("version 4\n", out)

    def test_with_option_refuses_twice_or_ambiguous(self):
        once = gate.with_option(self.ENTRY, gate.FAIL_HEALTH)
        with self.assertRaisesRegex(ValueError, "already"):
            gate.with_option(once, gate.FAIL_HEALTH)
        with self.assertRaisesRegex(ValueError, "exactly one"):
            gate.with_option(self.ENTRY + "options more\n", gate.FAIL_HEALTH)

    def test_with_default_replaces_the_default(self):
        out = gate.with_default(
            "timeout 0\ndefault flowerly-*\n", "manual-flowerly-a.conf"
        )
        self.assertEqual(out, "timeout 0\ndefault manual-flowerly-a.conf\n")
        with self.assertRaisesRegex(ValueError, "exactly one"):
            gate.with_default("timeout 0\n", "x")


class ReportReadingTests(unittest.TestCase):
    def test_last_activation_and_history(self):
        rows = [
            activation(1, "gb", "CONFIRMED"),
            activation(4, "ga", "FELL_BACK", kind="ROLLBACK"),
            activation(2, "ga", "CONFIRMED"),
        ]
        doc = report(entries=readable(rows))
        self.assertEqual(gate.last_activation(doc, "ga")["id"], 4)
        self.assertTrue(gate.history_readable(doc))
        with self.assertRaisesRegex(ValueError, "no activation"):
            gate.last_activation(doc, "gx")

    def test_unreadable_history_is_refused(self):
        entries = readable([])
        entries[1] = {
            "call": "history",
            "code": 1,
            "reply": None,
            "text": "cannot read",
        }
        doc = report(entries=entries)
        self.assertFalse(gate.history_readable(doc))
        with self.assertRaisesRegex(ValueError, "history unreadable"):
            gate.activations(doc)

    def test_refused_needs_nonzero_exit_and_reason(self):
        doc = report(
            entries=[
                {"call": "activate", "code": 1, "reply": None, "text": "needs ack"},
                {"call": "activate", "code": 0, "reply": {}, "text": ""},
            ]
        )
        self.assertEqual(gate.refused(doc, "activate", 0), "needs ack")
        with self.assertRaisesRegex(ValueError, "not refused"):
            gate.refused(doc, "activate")

    def test_a_refusal_for_another_reason_does_not_count(self):
        # Run 3: a SUPERSEDED Generation's refusal passed for the missing
        # acknowledgement's. The expected reason must be named.
        doc = report(
            entries=[
                {
                    "call": "activate",
                    "code": 1,
                    "reply": None,
                    "text": "activation needs a TESTED Generation",
                },
            ]
        )
        with self.assertRaisesRegex(ValueError, "not for FELL_BACK"):
            gate.refused(doc, "activate", because="FELL_BACK")
        doc["entries"][0]["text"] = "a FELL_BACK activation must be acknowledged"
        self.assertIn("FELL_BACK", gate.refused(doc, "activate", because="FELL_BACK"))

    def test_scenarios_cover_both_fall_backs(self):
        self.assertIn("unhealthy-activation-fell-back", gate.SCENARIOS)
        self.assertIn("unhealthy-rollback-fell-back", gate.SCENARIOS)

    def test_registered_generation(self):
        doc = report(entries=[{"call": "registered", "label": "B", "generation": "gb"}])
        self.assertEqual(gate.registered(doc, "B"), "gb")
        with self.assertRaisesRegex(ValueError, "did not register"):
            gate.registered(doc, "A")

    def test_rollback_kind_matches_flowerlyd(self):
        rows = [activation(5, "gb", "CONFIRMED", kind="ROLLBACK")]
        self.assertTrue(
            gate.is_confirmed(report(entries=readable(rows)), "gb", "ROLLBACK")
        )
        self.assertFalse(
            gate.is_confirmed(report(entries=readable(rows)), "gb", "ACTIVATE")
        )


class BootIdentityTests(unittest.TestCase):
    def test_witnessed_slot_and_marker(self):
        text = (
            "FLOWERLY_GENERATION_ROOT_VERIFIED\n"
            '{"slot": "root-b", "partuuid": "x"}\n'
            "FLOWERLY_GENERATION_USER_MARKER_SHA256=" + "9" * 64 + "\n"
        )
        self.assertEqual(gate.boot_identity(text), ("root-b", "9" * 64))

    def test_no_witness_or_two_slots(self):
        self.assertEqual(gate.boot_identity('{"slot": "root-a"}'), (None, None))
        both = 'FLOWERLY_GENERATION_ROOT_VERIFIED\n"slot": "root-a" "slot": "root-b"'
        self.assertEqual(gate.boot_identity(both), (None, None))

    def test_two_markers_refused(self):
        text = (
            'FLOWERLY_GENERATION_ROOT_VERIFIED\n"slot": "root-a"\n'
            "FLOWERLY_GENERATION_USER_MARKER_SHA256=" + "1" * 64 + "\n"
            "FLOWERLY_GENERATION_USER_MARKER_SHA256=" + "2" * 64 + "\n"
        )
        with self.assertRaisesRegex(ValueError, "more than one"):
            gate.boot_identity(text)


class SupervisorFailedTests(unittest.TestCase):
    """Run 8: systemd colours its status lines, so the plain phrase never
    matched and a failed supervisor read as started (and vice versa)."""

    COLOURED = (
        "[\x1b[0;1;31mFAILED\x1b[0m] Failed to start "
        "\x1b[0;1;39mFlowerly OS Unified Supervisor\x1b[0m.\r\n"
    )

    def test_a_coloured_status_line_is_seen(self):
        self.assertTrue(gate.supervisor_failed(self.COLOURED))

    def test_a_plain_status_line_is_seen(self):
        self.assertTrue(
            gate.supervisor_failed("Failed to start Flowerly OS Unified Supervisor.")
        )

    def test_a_started_supervisor_is_not_a_failure(self):
        self.assertFalse(
            gate.supervisor_failed(
                "[\x1b[0;32m  OK  \x1b[0m] Started "
                "\x1b[0;1;39mFlowerly OS Unified Supervisor\x1b[0m.\r\n"
                "Failed to start \x1b[0;1;39mSomething Else\x1b[0m.\r\n"
            )
        )


class ObserveTests(unittest.TestCase):
    """Run 4: /proc/<pid>/cmdline read right after Popen can still be empty
    while the exec completes, so the running QEMU is verified only once the
    firmware has written to the console, and an unverified boot fails."""

    def run_with(self, tmp, writes_after):
        run = object.__new__(gate.Run)
        run.out = Path(tmp)
        run.args = mock.Mock(ovmf_code="/code.fd")
        run.result = {"boots": []}
        run.ab = mock.Mock()
        log = run.out / "seen.serial.log"
        naps = []

        def verify(pid, command):
            # The shared check reads the real process; here it records when.
            self.assertGreater(log.stat().st_size, 0, "verified before output")
            return {"kvm_fd_verified": True}

        run.ab.verify_running_qemu.side_effect = verify

        def nap(_):
            naps.append(1)
            if len(naps) == writes_after:
                with log.open("a") as out:
                    out.write("BdsDxe: loading Boot0001\n")

        process = mock.Mock(pid=4242, returncode=0)
        process.poll.side_effect = lambda: None if len(naps) < 5 else 0
        with (
            mock.patch.object(gate.subprocess, "Popen", return_value=process),
            mock.patch.object(gate.time, "sleep", side_effect=nap),
            mock.patch.object(gate.shutil, "which", return_value="/usr/bin/qemu"),
        ):
            return run, run.observe("seen", "/disk.img", "/vars.fd")

    def test_the_process_is_verified_after_the_firmware_writes(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            run, record = self.run_with(tmp, writes_after=2)
        run.ab.verify_running_qemu.assert_called_once()
        self.assertTrue(record["kvm_fd_verified"])

    def test_a_boot_that_was_never_verified_fails(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaisesRegex(ValueError, "not verified"):
                self.run_with(tmp, writes_after=99)


class ValidateTests(unittest.TestCase):
    def test_execution_requires_token_and_clean_head(self):
        args = mock.Mock(approved_sha="0" * 39)
        with self.assertRaisesRegex(ValueError, "exact lowercase commit"):
            gate.validate(args)

    def test_scenarios_are_unique_and_complete(self):
        self.assertEqual(len(gate.SCENARIOS), len(set(gate.SCENARIOS)))
        for name in (
            "restore-drill",
            "downgrade-refused-across-schema",
            "fell-back-needs-acknowledgement",
            "exhausted-target-observed",
            "history-readable-every-step",
            "user-data-intact",
        ):
            self.assertIn(name, gate.SCENARIOS)


if __name__ == "__main__":
    unittest.main()
