#!/usr/bin/env python3
"""Offline installation and kernel/EFI witness regression for opt-in A/B guest."""

import hashlib
import importlib.util
import json
import os
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
BOOT_2 = "44444444-4444-4444-4444-444444444444"
GOLDEN = ROOT / "testbed/generation-ledger/v2-golden.jsonl"


def identity(slot="root-a", partuuid=UUID_A):
    return {
        "schema": "flowerly-generation-root/v1",
        "slot": slot,
        "root_partuuid": partuuid,
        "source_commit": COMMIT,
        "input_manifest_sha256": MANIFEST,
        "build_id": "fixture-a",
    }


def efi(slot="a"):
    return b"\x07\x00\x00\x00" + ("flowerly-" + slot + "+3.conf").encode("utf-16-le")


def create_root(root):
    for file in (
        "usr/lib/os-release",
        "usr/bin/python3",
        "usr/bin/runuser",
        "usr/lib/systemd/systemd-bless-boot",
        "usr/lib/systemd/system/flowerlyd.service",
        "usr/lib/flowerly/boot-ipc-health.sh",
        "usr/lib/sysusers.d/flowerly.conf",
    ):
        target = root / file
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text("ID=arch\n" if file.endswith("os-release") else "fixture\n")


def prepare(root, partuuid=UUID_A):
    cmd = [
        "bash",
        str(INSTALLER),
        str(root),
        "--slot",
        "root-a",
        "--partuuid",
        partuuid,
        "--source-commit",
        COMMIT,
        "--input-manifest-sha256",
        MANIFEST,
        "--build-id",
        "fixture-a",
        "--test-profile",
    ]
    fake = Path(root).parent / (Path(root).name + "-sysusers")
    fake.write_text(
        '#!/bin/sh\nr="${1#--root=}"\nmkdir -p "$r/etc" && echo baked > "$r/etc/passwd"\n'
    )
    fake.chmod(0o755)
    env = dict(os.environ, FLOWERLY_SYSUSERS=str(fake))
    return subprocess.run(cmd, capture_output=True, text=True, env=env)


class BootTests(unittest.TestCase):
    def test_verified_efi_kernel_and_baked_identity(self):
        with tempfile.TemporaryDirectory() as d:
            ledger = Path(d) / "ledger.jsonl"
            witness = guest.append_witness(
                ledger,
                identity(),
                "console=ttyS0 root=PARTUUID=" + UUID_A,
                efi("a"),
                BOOT,
                root_device=10,
                data_device=20,
            )
            self.assertEqual(witness["slot"], "root-a")
            self.assertEqual(witness["kind"], "witness")
            self.assertNotIn("health", witness)
            self.assertEqual(witness["sequence"], 1)
            self.assertEqual(witness["prev_sha256"], "0" * 64)
            self.assertEqual(len(ledger.read_text().splitlines()), 1)
            with self.assertRaises(ValueError):
                guest.append_witness(
                    ledger,
                    identity(),
                    "root=PARTUUID=" + UUID_A,
                    efi("a"),
                    BOOT,
                    10,
                    20,
                )

    def test_persistent_marker_survives_reboots_and_missing_data_fails(self):
        with tempfile.TemporaryDirectory() as d:
            ledger = Path(d) / "ledger.jsonl"
            marker = Path(d) / "user-marker.bin"
            a = guest.append_witness(
                ledger,
                identity(),
                "root=PARTUUID=" + UUID_A,
                efi("a"),
                BOOT,
                10,
                20,
                marker_path=marker,
            )
            self.assertEqual(marker.stat().st_size, 32)
            boot2 = "44444444-4444-4444-4444-444444444444"
            b = guest.append_witness(
                ledger,
                identity("root-b", UUID_B),
                "root=PARTUUID=" + UUID_B,
                efi("b"),
                boot2,
                10,
                20,
                marker_path=marker,
            )
            self.assertEqual(a["user_data_sha256"], b["user_data_sha256"])
            marker.unlink()
            with self.assertRaisesRegex(ValueError, "marker missing"):
                guest.append_witness(
                    ledger,
                    identity(),
                    "root=PARTUUID=" + UUID_A,
                    efi("a"),
                    "55555555-5555-5555-5555-555555555555",
                    10,
                    20,
                    marker_path=marker,
                )

    def witness_a(self, ledger, boot=BOOT, **kwargs):
        return guest.append_witness(
            ledger,
            identity(),
            "root=PARTUUID=" + UUID_A,
            efi("a"),
            boot,
            10,
            20,
            **kwargs,
        )

    def test_records_are_chained_and_health_follows_its_witness(self):
        with tempfile.TemporaryDirectory() as d:
            ledger = Path(d) / "ledger.jsonl"
            self.witness_a(ledger)
            health = guest.append_health(ledger, BOOT)
            self.assertEqual(
                (health["kind"], health["status"], health["boot_id"]),
                ("health", "PASSED", BOOT),
            )
            self.witness_a(ledger, BOOT_2)
            lines = ledger.read_bytes().splitlines()
            head = "0" * 64
            for number, line in enumerate(lines, start=1):
                record = json.loads(line)
                self.assertEqual(record["sequence"], number)
                self.assertEqual(record["prev_sha256"], head)
                self.assertEqual(record["schema"], "flowerly-generation-boot/v2")
                head = hashlib.sha256(line).hexdigest()

    def test_health_refusals_do_not_modify_ledger(self):
        with tempfile.TemporaryDirectory() as d:
            ledger = Path(d) / "ledger.jsonl"
            with self.assertRaises(ValueError):
                guest.append_health(ledger, BOOT)
            self.witness_a(ledger)
            before = ledger.read_bytes()
            for boot in (BOOT_2, "not-a-boot-id"):
                with self.subTest(boot=boot), self.assertRaises(ValueError):
                    guest.append_health(ledger, boot)
            self.assertEqual(ledger.read_bytes(), before)
            guest.append_health(ledger, BOOT)
            after = ledger.read_bytes()
            with self.assertRaisesRegex(ValueError, "health"):
                guest.append_health(ledger, BOOT)
            self.assertEqual(ledger.read_bytes(), after)

    def test_a_broken_or_torn_chain_is_never_extended(self):
        with tempfile.TemporaryDirectory() as d:
            ledger = Path(d) / "ledger.jsonl"
            self.witness_a(ledger)
            guest.append_health(ledger, BOOT)
            good = ledger.read_bytes()
            first, rest = good.split(b"\n", 1)
            for damaged in (
                first.replace(b"fixture-a", b"fixture-z") + b"\n" + rest,
                good[:-1],
                rest,
                good.replace(
                    b"flowerly-generation-boot/v2", b"flowerly-generation-boot/v1"
                ),
            ):
                ledger.write_bytes(damaged)
                with self.subTest(damaged=damaged[:40]):
                    with self.assertRaises(ValueError):
                        self.witness_a(ledger, BOOT_2)
                    with self.assertRaises(ValueError):
                        guest.append_health(ledger, BOOT)
                    self.assertEqual(ledger.read_bytes(), damaged)

    def test_owner_and_mode_let_only_the_service_group_read(self):
        with tempfile.TemporaryDirectory() as d:
            ledger = Path(d) / "ledger.jsonl"
            owner = (os.getuid(), os.getgid())
            self.witness_a(ledger, owner=owner)
            self.assertEqual(ledger.stat().st_mode & 0o7777, 0o640)
            self.assertEqual((ledger.stat().st_uid, ledger.stat().st_gid), owner)
            os.chmod(ledger, 0o666)
            guest.append_health(ledger, BOOT, owner=owner)
            self.assertEqual(ledger.stat().st_mode & 0o7777, 0o640)

    def test_writer_output_matches_the_golden_ledger_the_reader_parses(self):
        with tempfile.TemporaryDirectory() as d:
            ledger = Path(d) / "ledger.jsonl"
            self.witness_a(ledger)
            guest.append_health(ledger, BOOT)
            guest.append_witness(
                ledger,
                identity("root-b", UUID_B),
                "root=PARTUUID=" + UUID_B,
                efi("b"),
                BOOT_2,
                10,
                20,
            )
            self.assertEqual(ledger.read_bytes(), GOLDEN.read_bytes())

    def test_refusals_do_not_modify_ledger(self):
        with tempfile.TemporaryDirectory() as d:
            ledger = Path(d) / "ledger.jsonl"
            bad = [
                (identity(), "root=PARTUUID=" + UUID_A, efi("b"), 1, 2),
                (identity(), "root=PARTUUID=" + UUID_B, efi("a"), 1, 2),
                (
                    identity(),
                    "root=PARTUUID=" + UUID_A + " root=PARTUUID=" + UUID_B,
                    efi("a"),
                    1,
                    2,
                ),
                (identity(), "root=PARTUUID=" + UUID_A, efi("a"), 1, 1),
                (identity(), "root=PARTUUID=" + UUID_A, b"\x00\x00", 1, 2),
            ]
            for ident, cmdline, selected, rootdev, datadev in bad:
                with (
                    self.subTest(cmdline=cmdline, efi=selected),
                    self.assertRaises(ValueError),
                ):
                    guest.append_witness(
                        ledger, ident, cmdline, selected, BOOT, rootdev, datadev
                    )
            self.assertFalse(ledger.exists())
            symlink_target = Path(d) / "sensitive"
            symlink_target.write_text("private\n")
            ledger.symlink_to(symlink_target)
            with self.assertRaises(ValueError):
                guest.append_witness(
                    ledger, identity(), "root=PARTUUID=" + UUID_A, efi("a"), BOOT, 1, 2
                )
            self.assertEqual(symlink_target.read_text(), "private\n")

    def test_install_is_opt_in_and_generation_units_are_ordered(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d) / "root"
            create_root(root)
            result = prepare(root)
            self.assertEqual(result.returncode, 0, result.stderr)
            unit_dir = root / "usr/lib/systemd/system"
            wants = root / "etc/systemd/system/multi-user.target.wants"
            for name in ("flowerly-generation-witness", "flowerly-generation-health"):
                self.assertTrue((unit_dir / (name + ".service")).is_file())
                self.assertTrue((wants / (name + ".service")).is_symlink())
            self.assertEqual(
                json.loads(
                    (root / "usr/lib/flowerly/generation-root.json").read_text()
                )["root_partuuid"],
                UUID_A,
            )
            self.assertIn(
                "PARTLABEL=data /var/lib/flowerly", (root / "etc/fstab").read_text()
            )
            self.assertIn("PARTLABEL=esp /efi vfat", (root / "etc/fstab").read_text())
            self.assertTrue((root / "efi").is_dir())
            bless = root / "usr/lib/flowerly/generation-bless.sh"
            self.assertTrue(bless.is_file() and os.access(bless, os.X_OK))
            self.assertEqual((root / "etc/passwd").read_text(), "baked\n")
            mask = root / "etc/systemd/system/systemd-bless-boot.service"
            self.assertEqual(mask.readlink(), Path("/dev/null"))
            self.assertIn(
                "After=flowerly-generation-witness.service",
                (
                    unit_dir / "flowerlyd.service.d/10-generation-witness.conf"
                ).read_text(),
            )
            health_unit = (unit_dir / "flowerly-generation-health.service").read_text()
            self.assertIn("generation resolve", health_unit)
            self.assertLess(
                health_unit.index("FLOWERLY_GENERATION_BOOT_BLESSED"),
                health_unit.index("generation resolve"),
            )
            gate = (root / "usr/lib/flowerly/generation-health-gate.sh").read_text()
            self.assertLess(
                gate.index("boot-ipc-health.sh"),
                gate.index("generation-ab-guest.py health"),
            )
            self.assertNotEqual(
                prepare(root).returncode, 0, "duplicate install accepted"
            )

    def test_arch_internal_prerequisite_link_is_accepted(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "root"
            create_root(root)
            python = root / "usr/bin/python3"
            python.rename(root / "usr/bin/python3.14")
            python.symlink_to("python3.14")
            self.assertEqual(prepare(root).returncode, 0)

    def test_external_prerequisite_link_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "root"
            create_root(root)
            python = root / "usr/bin/python3"
            python.unlink()
            python.symlink_to("/usr/bin/false")
            self.assertNotEqual(prepare(root).returncode, 0)

    def test_installer_refuses_untrusted_inputs(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d) / "root"
            create_root(root)
            self.assertNotEqual(prepare(root, "bad").returncode, 0)
            (root / "etc").mkdir(parents=True)
            (root / "etc/fstab").write_text(
                "PARTLABEL=else /var/lib/flowerly ext4 defaults 0 0\n"
            )
            self.assertNotEqual(prepare(root).returncode, 0)


if __name__ == "__main__":
    unittest.main()
