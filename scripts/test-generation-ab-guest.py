#!/usr/bin/env python3
"""Offline installation and kernel/EFI witness regression for opt-in A/B guest."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
INSTALLER = ROOT / "scripts/prepare-generation-ab-rootfs.sh"
GUEST = ROOT / "distro/overlay/usr/lib/flowerly/generation-ab-guest.py"
spec = importlib.util.spec_from_file_location("generation_guest", GUEST)
guest = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guest)
UUID_A = "11111111-1111-1111-1111-111111111111"
UUID_B = "22222222-2222-2222-2222-222222222222"
COMMIT = "a" * 40
MANIFEST = "b" * 64
BOOT = "33333333-3333-3333-3333-333333333333"


def identity(slot="root-a", partuuid=UUID_A):
    return {"schema": "flowerly-generation-root/v1", "slot": slot,
            "root_partuuid": partuuid, "source_commit": COMMIT,
            "input_manifest_sha256": MANIFEST, "build_id": "fixture-a"}


def efi(slot="a"):
    return b"\x07\x00\x00\x00" + ("flowerly-"+slot+"+3.conf").encode("utf-16-le")


def create_root(root):
    for file in ("usr/lib/os-release", "usr/bin/python3", "usr/bin/runuser",
                 "usr/lib/systemd/systemd-bless-boot",
                 "usr/lib/systemd/system/flowerlyd.service",
                 "usr/lib/flowerly/boot-ipc-health.sh"):
        target = root / file
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text("ID=arch\n" if file.endswith("os-release") else "fixture\n")


def prepare(root, partuuid=UUID_A):
    cmd = ["bash", str(INSTALLER), str(root), "--slot", "root-a",
           "--partuuid", partuuid, "--source-commit", COMMIT,
           "--input-manifest-sha256", MANIFEST, "--build-id", "fixture-a",
           "--test-profile"]
    return subprocess.run(cmd, capture_output=True, text=True)


class BootTests(unittest.TestCase):
    def test_verified_efi_kernel_and_baked_identity(self):
        with tempfile.TemporaryDirectory() as d:
            ledger = Path(d)/"ledger.jsonl"
            witness = guest.append_witness(ledger, identity(),
                        "console=ttyS0 root=PARTUUID="+UUID_A,
                        efi("a"), BOOT, root_device=10, data_device=20)
            self.assertEqual(witness["slot"], "root-a")
            self.assertEqual(witness["health"], "NOT_YET_TESTED")
            self.assertEqual(len(ledger.read_text().splitlines()), 1)
            with self.assertRaises(ValueError):
                guest.append_witness(ledger, identity(), "root=PARTUUID="+UUID_A,
                                     efi("a"), BOOT, 10, 20)

    def test_persistent_marker_survives_reboots_and_missing_data_fails(self):
        with tempfile.TemporaryDirectory() as d:
            ledger = Path(d) / "ledger.jsonl"
            marker = Path(d) / "user-marker.bin"
            a = guest.append_witness(
                ledger, identity(), "root=PARTUUID="+UUID_A,
                efi("a"), BOOT, 10, 20, marker_path=marker)
            self.assertEqual(marker.stat().st_size, 32)
            boot2 = "44444444-4444-4444-4444-444444444444"
            b = guest.append_witness(
                ledger, identity("root-b", UUID_B),
                "root=PARTUUID="+UUID_B, efi("b"), boot2, 10, 20,
                marker_path=marker)
            self.assertEqual(a["user_data_sha256"], b["user_data_sha256"])
            marker.unlink()
            with self.assertRaisesRegex(ValueError, "marker missing"):
                guest.append_witness(
                    ledger, identity(), "root=PARTUUID="+UUID_A,
                    efi("a"), "55555555-5555-5555-5555-555555555555",
                    10, 20, marker_path=marker)

    def test_refusals_do_not_modify_ledger(self):
        with tempfile.TemporaryDirectory() as d:
            ledger = Path(d)/"ledger.jsonl"
            bad = [
                (identity(), "root=PARTUUID="+UUID_A, efi("b"), 1, 2),
                (identity(), "root=PARTUUID="+UUID_B, efi("a"), 1, 2),
                (identity(), "root=PARTUUID="+UUID_A+" root=PARTUUID="+UUID_B, efi("a"), 1, 2),
                (identity(), "root=PARTUUID="+UUID_A, efi("a"), 1, 1),
                (identity(), "root=PARTUUID="+UUID_A, b"\x00\x00", 1, 2),
            ]
            for ident, cmdline, selected, rootdev, datadev in bad:
                with self.subTest(cmdline=cmdline, efi=selected), self.assertRaises(ValueError):
                    guest.append_witness(ledger, ident, cmdline, selected, BOOT,
                                         rootdev, datadev)
            self.assertFalse(ledger.exists())
            symlink_target = Path(d)/"sensitive"
            symlink_target.write_text("private\n")
            ledger.symlink_to(symlink_target)
            with self.assertRaises(ValueError):
                guest.append_witness(ledger, identity(), "root=PARTUUID="+UUID_A,
                                     efi("a"), BOOT, 1, 2)
            self.assertEqual(symlink_target.read_text(), "private\n")

    def test_install_is_opt_in_and_generation_units_are_ordered(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)/"root"
            create_root(root)
            result = prepare(root)
            self.assertEqual(result.returncode, 0, result.stderr)
            unit_dir = root/"usr/lib/systemd/system"
            wants = root/"etc/systemd/system/multi-user.target.wants"
            for name in ("flowerly-generation-witness", "flowerly-generation-health"):
                self.assertTrue((unit_dir/(name+".service")).is_file())
                self.assertTrue((wants/(name+".service")).is_symlink())
            self.assertEqual(json.loads((root/"usr/lib/flowerly/generation-root.json").read_text())["root_partuuid"], UUID_A)
            self.assertIn("PARTLABEL=data /var/lib/flowerly",
                          (root/"etc/fstab").read_text())
            mask = root/"etc/systemd/system/systemd-bless-boot.service"
            self.assertEqual(mask.readlink(), Path("/dev/null"))
            self.assertIn("After=flowerly-generation-witness.service",
                          (unit_dir/"flowerlyd.service.d/10-generation-witness.conf").read_text())
            self.assertNotEqual(prepare(root).returncode, 0, "duplicate install accepted")

    def test_installer_refuses_untrusted_inputs(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)/"root"
            create_root(root)
            self.assertNotEqual(prepare(root, "bad").returncode, 0)
            (root/"etc").mkdir(parents=True)
            (root/"etc/fstab").write_text("PARTLABEL=else /var/lib/flowerly ext4 defaults 0 0\n")
            self.assertNotEqual(prepare(root).returncode, 0)


if __name__ == "__main__":
    unittest.main()
