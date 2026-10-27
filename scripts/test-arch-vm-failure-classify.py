#!/usr/bin/env python3
"""Offline fixtures for arch-vm-failure-classify.py (#184)."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("arch-vm-failure-classify.py")
spec = importlib.util.spec_from_file_location("classify", SCRIPT)
classify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(classify)

BOOT = "4c1e0f6a-1111-4222-8333-944455556666"


def run(serial="", **overrides):
    arguments = {"exit-status": 1, "stage": "verify", "ssh-status": 1, "qemu": "alive",
                 "guest-reachable": "yes", "oom-kill-start": 0, "oom-kill-end": 0,
                 "boot-id-start": BOOT, "boot-id-end": BOOT}
    arguments.update({key.replace("_", "-"): value for key, value in overrides.items()})
    with tempfile.TemporaryDirectory() as directory:
        report = Path(directory)
        (report / "qemu-serial.log").write_text(serial)
        argv = ["--report-dir", str(report)]
        for key, value in arguments.items():
            if value is not None:
                argv += [f"--{key}", str(value)]
        assert classify.main(argv) == 0
        return (report / "failure-classification.txt").read_text()


def category(text):
    return text.splitlines()[0].split("=", 1)[1]


class Classification(unittest.TestCase):
    def test_success(self):
        self.assertEqual(category(run(exit_status=0, ssh_status=0)), "success")

    def test_host_reboot_wins(self):
        self.assertEqual(category(run(boot_id_end="0" * 36, qemu="absent")), "host_reboot")

    def test_host_oom_needs_qemu_gone_and_cgroup_oom_kill(self):
        self.assertEqual(category(run(qemu="absent", oom_kill_end=1, ssh_status=255)), "host_oom")
        self.assertEqual(category(run(qemu="no_pidfile", ssh_status=255)), "qemu_failure")

    def test_guest_oom_and_crash_from_serial(self):
        oom = "[ 812.1] Out of memory: Killed process 4242 (cargo) total-vm:1kB\n"
        self.assertEqual(category(run(oom, ssh_status=255, guest_reachable="no")), "guest_oom")
        panic = "\x1b[0;31m[ 900.0] Kernel panic - not syncing: Fatal exception\x1b[0m\n"
        text = run(panic, ssh_status=255, guest_reachable="no")
        self.assertEqual(category(text), "guest_crash")
        self.assertIn("guest_crash_line=[ 900.0] Kernel panic - not syncing", text)
        self.assertNotIn("\x1b", text)

    def test_clean_shutdown_watchdog_notice_is_not_a_crash(self):
        # Seen in the real 154da3d run: poweroff prints this, plus OSC/CSI noise.
        serial = ("[  273.881246] watchdog: watchdog0: watchdog did not stop!\n"
                  "\x1b]3008;end=007d21dc\x1b\\[  274.2] watchdog: watchdog0: watchdog did not stop!\n"
                  "\x1b[18t\x1b7\x1b[32766;32766H\x1b[6n\x1b8[ 275.7] reboot: Power down\n")
        text = run(serial, ssh_status=1)
        self.assertEqual(category(text), "test_failure")
        self.assertNotIn("guest_crash_line", text)
        lockup = "[ 99.0] watchdog: BUG: soft lockup - CPU#1 stuck for 22s!\n"
        self.assertEqual(category(run(lockup, ssh_status=1)), "guest_crash")

    def test_terminal_escapes_are_removed_from_evidence(self):
        text = run("\x1b]3008;start=abc\x1b\\\x1b[0;31m[ 1.0] Kernel panic - x\x1b[0m\n")
        line = next(l for l in text.splitlines() if l.startswith("guest_crash_line="))
        self.assertEqual(line, "guest_crash_line=[ 1.0] Kernel panic - x")

    def test_unreachable_live_guest_is_a_crash(self):
        self.assertEqual(category(run(ssh_status=255, guest_reachable="no")), "guest_crash")

    def test_ssh_interruption_and_test_failure(self):
        self.assertEqual(category(run(ssh_status=255)), "ssh_interruption")
        self.assertEqual(category(run(ssh_status=1)), "test_failure")

    def test_failure_before_the_vm_is_not_blamed_on_qemu(self):
        self.assertEqual(category(run(stage="before-vm", qemu="no_pidfile", ssh_status=None,
                                      guest_reachable="unknown")), "unknown")

    def test_serial_evidence_is_bounded_and_printable(self):
        serial = "".join(f"[ {i}.0] Out of memory: Killed process {i} \x07\x00 {'x' * 400}\n"
                         for i in range(50))
        text = run(serial)
        kept = [line for line in text.splitlines() if line.startswith("guest_oom_line=")]
        self.assertEqual(len(kept), 20)
        self.assertTrue(all(len(line) <= len("guest_oom_line=") + 200 for line in kept))
        self.assertTrue(all(32 <= ord(ch) < 127 for line in kept for ch in line))


if __name__ == "__main__":
    unittest.main()
