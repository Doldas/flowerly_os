#!/usr/bin/env python3
"""Offline tests for direct QEMU boot preflight and serial acceptance."""
import importlib.util
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

SCRIPT = Path(__file__).with_name("probe-arch-qemu-boot.py")
spec = importlib.util.spec_from_file_location("boot_probe", SCRIPT)
boot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(boot)


class BootProbeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.base = Path(self.tmp.name)
        self.rootfs = self.base / "rootfs"
        for name, body in (
            ("usr/lib/os-release", "ID=arch\n"),
            ("usr/lib/systemd/systemd", "systemd"),
            ("usr/bin/flowerlyd", "binary"),
            ("boot/vmlinuz-linux", "kernel"),
            ("boot/initramfs-linux.img", "initramfs"),
        ):
            dest = self.rootfs / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_text(body)
        self.volume = self.base / "root.ext4"
        self.volume.write_bytes(b"ext4 fixture")
        self.args = mock.Mock(rootfs=self.rootfs, volume=self.volume,
                              log=self.base / "serial.log", timeout=10, report=None)

    def test_packaged_kernel_path_accepted(self):
        packaged = self.rootfs / "usr/lib/modules/7.2.11-arch1-1/vmlinuz"
        packaged.parent.mkdir(parents=True)
        (self.rootfs / "boot/vmlinuz-linux").rename(packaged)
        self.simulate("[  OK  ] Reached target Multi-User System.\nFLOWERLY_BOOT_IPC_HEALTHY\n")

    def test_ambiguous_kernels_rejected(self):
        packaged = self.rootfs / "usr/lib/modules/7.2.11-arch1-1/vmlinuz"
        packaged.parent.mkdir(parents=True)
        packaged.write_bytes(b"kernel")
        with self.assertRaisesRegex(ValueError, "exactly one kernel"):
            boot.run(self.args)

    def test_missing_initramfs_rejected(self):
        (self.rootfs / "boot/initramfs-linux.img").unlink()
        with self.assertRaisesRegex(ValueError, "Build and stage an Arch initramfs"):
            boot.run(self.args)

    def test_wrong_distro_rejected(self):
        (self.rootfs / "usr/lib/os-release").write_text("ID=debian\n")
        with self.assertRaisesRegex(ValueError, "Arch Linux"):
            boot.run(self.args)

    def test_preexisting_log_rejected(self):
        self.args.log.write_text("prior boot")
        with mock.patch.object(boot.shutil, "which", return_value="/usr/bin/qemu-system-x86_64"):
            with self.assertRaisesRegex(ValueError, "overwrite serial log"):
                boot.run(self.args)

    def simulate(self, transcript):
        class FakeQemu:
            returncode = None

            def __init__(self, _command, **kwargs):
                kwargs["stdout"].write(transcript.encode())
                kwargs["stdout"].flush()

            def wait(self, timeout=None):
                if timeout == 10:
                    raise subprocess.TimeoutExpired("qemu", 10)
                return 0

            def terminate(self):
                return None

            def kill(self):
                return None

        with mock.patch.object(boot.shutil, "which", return_value="/usr/bin/qemu-system-x86_64"), \
             mock.patch.object(boot.subprocess, "Popen", FakeQemu):
            boot.run(self.args)

    def test_systemd_multi_user_accepted(self):
        self.simulate("[  OK  ] Reached target Multi-User System.\nFLOWERLY_BOOT_IPC_HEALTHY\n")

    def test_witness_mentioned_in_diagnostic_rejected(self):
        with self.assertRaisesRegex(ValueError, "IPC health witness not observed"):
            self.simulate("[  OK  ] Reached target Multi-User System.\n"
                          "Boot witness expects FLOWERLY_BOOT_IPC_HEALTHY\n")

    def test_partial_witness_rejected(self):
        with self.assertRaisesRegex(ValueError, "IPC health witness not observed"):
            self.simulate("[  OK  ] Reached target Multi-User System.\n"
                          "FLOWERLY_BOOT_IPC_HEALTHY_NOT_YET\n")

    def test_crlf_witness_accepted(self):
        self.simulate("[  OK  ] Reached target Multi-User System.\r\n"
                      "FLOWERLY_BOOT_IPC_HEALTHY\r\n")

    def test_evidence_report_contains_artifact_hashes(self):
        self.args.report = self.base / "evidence.json"
        self.simulate("[  OK  ] Reached target Multi-User System.\n"
                      "FLOWERLY_BOOT_IPC_HEALTHY\n")
        evidence = json.loads(self.args.report.read_text())
        self.assertTrue(evidence["accepted"])
        self.assertEqual(evidence["schema"], 1)
        self.assertEqual(evidence["artifacts"]["volume"]["sha256"],
                         hashlib.sha256(self.volume.read_bytes()).hexdigest())
        self.assertEqual(evidence["artifacts"]["serial_log"]["sha256"],
                         hashlib.sha256(self.args.log.read_bytes()).hexdigest())
        self.assertIn("elapsed_seconds", evidence)

    def test_evidence_report_never_overwritten(self):
        self.args.report = self.base / "evidence.json"
        self.args.report.write_text("prior evidence")
        with mock.patch.object(boot.shutil, "which", return_value="/usr/bin/qemu-system-x86_64"):
            with self.assertRaisesRegex(ValueError, "overwrite evidence report"):
                boot.run(self.args)
        self.assertEqual(self.args.report.read_text(), "prior evidence")

    def test_kernel_panic_rejected(self):
        with self.assertRaisesRegex(ValueError, "kernel panic"):
            self.simulate("Kernel panic - not syncing\nReached target Multi-User System\n")

    def test_missing_flowerly_witness_rejected(self):
        with self.assertRaisesRegex(ValueError, "IPC health witness not observed"):
            self.simulate("[  OK  ] Reached target Multi-User System.\\n")

    def test_required_docs_gate_negative_proof_issue_55(self):
        # INTENTIONAL FAILURE: #55 verifies that required docs blocks a non-draft PR.
        # Do not merge this test into release/2. Close the PR after capturing proof.
        self.fail("EXPECTED_NEGATIVE_GATE_ISSUE_55_DO_NOT_MERGE")

    def test_no_boot_confirmation_rejected(self):
        with self.assertRaisesRegex(ValueError, "not observed"):
            self.simulate("Booting Linux...\n")


if __name__ == "__main__":
    unittest.main()
