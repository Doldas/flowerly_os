#!/usr/bin/env python3
"""Offline regression of OVMF/KVM evidence parser and immutable-root carryover."""
import hashlib
import importlib.util
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location(
    "kvm", HERE / "generation-ab-kvm-acceptance.py")
kvm = importlib.util.module_from_spec(spec)
spec.loader.exec_module(kvm)
MIB = 1048576
H = "a" * 64


def log_text(slot, healthy):
    lines = ["FLOWERLY_GENERATION_ROOT_VERIFIED",
             '{"boot_id": "example", "slot": "' + slot + '"}',
             "FLOWERLY_GENERATION_USER_MARKER_SHA256=" + H]
    if healthy:
        lines += ["FLOWERLY_GENERATION_IPC_VERIFIED",
                  "FLOWERLY_GENERATION_BOOT_BLESSED"]
    else:
        lines += ["injected unhealthy Generation boot"]
    return "\n".join(lines) + "\n"


class KvmGateTests(unittest.TestCase):
    def test_boot_bless_and_refusal_outcomes(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "serial.log"
            path.write_text(log_text("root-a", True))
            self.assertEqual(kvm.observe_serial(path, "root-a", bless=True, timeout=20), H)
            self.assertIsNone(kvm.observe_serial(path, "root-b", bless=True, timeout=20))
            path.write_text(log_text("root-b", False))
            self.assertEqual(kvm.observe_serial(path, "root-b", bless=False, timeout=20), H)
            self.assertIsNone(kvm.observe_serial(path, "root-b", bless=True, timeout=20))
            path.write_text(log_text("root-b", False) + "FLOWERLY_GENERATION_BOOT_BLESSED\n")
            self.assertIsNone(kvm.observe_serial(path, "root-b", bless=False, timeout=20))
            path.write_text("FLOWERLY_GENERATION_BOOT_BLESSED\n")
            self.assertIsNone(kvm.observe_serial(path, "root-a", bless=True, timeout=20))

    def test_only_esp_and_user_data_are_carried_between_guests(self):
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            pristine = folder / "pristine.img"
            booted = folder / "after-boot-a.img"
            dest = folder / "reconciled.img"
            a = b"".join(bytes([i]) * MIB for i in range(7))
            b = bytearray(a)
            b[MIB+4096] = 200            # EFI boot-count rename metadata
            b[5*MIB+512] = 201          # persistent user-data marker
            b[2*MIB+2048] = 202         # incidental rootfs ext4 write
            pristine.write_bytes(a)
            booted.write_bytes(b)
            manifest = {"partitions": [
                {"name": name, "offset_bytes": i*MIB,
                 "size_bytes": (2 if name == "root-b" else 1)*MIB}
                for i, name in ((1, "esp"), (2, "root-a"),
                                (3, "root-b"), (5, "data"))
            ]}
            # Only the two explicitly mutable sections are copied.
            hashes = kvm.reconcile_mutable_partitions(
                pristine, booted, dest, manifest)
            result = dest.read_bytes()
            self.assertEqual(result[MIB:2*MIB], bytes(b[MIB:2*MIB]))
            self.assertEqual(result[5*MIB:6*MIB], bytes(b[5*MIB:6*MIB]))
            self.assertEqual(result[2*MIB:5*MIB], a[2*MIB:5*MIB])
            self.assertEqual(hashes["data"], hashlib.sha256(b[5*MIB:6*MIB]).hexdigest())

    def test_stale_commit_fails_before_touching_disk_or_kvm(self):
        args = SimpleNamespace(approved_sha="f"*40)
        with self.assertRaises(ValueError):
            kvm.validate_source(args, actual_execution=False)


if __name__ == "__main__":
    unittest.main()
