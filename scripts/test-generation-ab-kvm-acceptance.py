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
    "kvm", HERE / "generation-ab-kvm-acceptance.py"
)
kvm = importlib.util.module_from_spec(spec)
spec.loader.exec_module(kvm)
audit_spec = importlib.util.spec_from_file_location(
    "audit", HERE / "generation-ab-evidence-audit.py"
)
audit = importlib.util.module_from_spec(audit_spec)
audit_spec.loader.exec_module(audit)
MIB = 1048576
H = "a" * 64


def log_text(slot, healthy):
    lines = [
        "FLOWERLY_GENERATION_ROOT_VERIFIED",
        '{"boot_id": "example", "slot": "' + slot + '"}',
        "FLOWERLY_GENERATION_USER_MARKER_SHA256=" + H,
    ]
    if healthy:
        lines += [
            "FLOWERLY_GENERATION_IPC_VERIFIED",
            "FLOWERLY_GENERATION_BOOT_BLESSED",
        ]
    else:
        lines += ["injected unhealthy Generation boot"]
    return "\n".join(lines) + "\n"


class KvmGateTests(unittest.TestCase):
    def test_boot_bless_and_refusal_outcomes(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "serial.log"
            path.write_text(log_text("root-a", True))
            self.assertEqual(
                kvm.observe_serial(path, "root-a", bless=True, timeout=20), H
            )
            self.assertIsNone(
                kvm.observe_serial(path, "root-b", bless=True, timeout=20)
            )
            path.write_text(log_text("root-b", False))
            self.assertEqual(
                kvm.observe_serial(path, "root-b", bless=False, timeout=20), H
            )
            self.assertIsNone(
                kvm.observe_serial(path, "root-b", bless=True, timeout=20)
            )
            path.write_text(
                log_text("root-b", False) + "FLOWERLY_GENERATION_BOOT_BLESSED\n"
            )
            self.assertIsNone(
                kvm.observe_serial(path, "root-b", bless=False, timeout=20)
            )
            path.write_text("FLOWERLY_GENERATION_BOOT_BLESSED\n")
            self.assertIsNone(
                kvm.observe_serial(path, "root-a", bless=True, timeout=20)
            )

    def test_end_to_end_serial_audit_and_tampered_evidence(self):
        import copy
        import json

        root_a = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
        root_b = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
        tries = 2
        plan = [("boot-a", "root-a", "BLESSED", "main")]
        plan += [(f"unhealthy-b-{i}", "root-b", "REJECTED", "main") for i in (1, 2)]
        plan += [
            ("auto-fallback-a", "root-a", "BLESSED", "main"),
            ("healthy-b-1", "root-b", "BLESSED", "main"),
            ("healthy-b-2", "root-b", "BLESSED", "main"),
            ("rollback-a", "root-a", "BLESSED", "main"),
            ("arm-before-rename-a", "root-a", "BLESSED", "main"),
            ("arm-after-rename-a", "root-a", "BLESSED", "main"),
        ]
        plan += [(f"damaged-b-{i}", "root-b", "NO_WITNESS", "main") for i in (1, 2)]
        plan += [
            ("damaged-fallback-a", "root-a", "BLESSED", "main"),
            ("stage-interrupted-a", "root-a", "BLESSED", "stage-interrupted"),
        ]
        marker_of = {"main": H, "stage-interrupted": "d" * 64}
        with tempfile.TemporaryDirectory() as d:
            folder = Path(d)
            boots, checksums = [], {}
            for index, (label, slot, expect, lineage) in enumerate(plan):
                root = root_a if slot == "root-a" else root_b
                lines = [
                    "[    0.0] Kernel command line: initrd=\\flowerly\\"
                    + slot[-1]
                    + "\\initramfs.img root=PARTUUID="
                    + root
                ]
                if expect != "NO_WITNESS":
                    lines += [
                        "FLOWERLY_GENERATION_ROOT_VERIFIED",
                        '{"boot_id": "'
                        + f"{index:08d}-1111-1111-1111-111111111111"
                        + '", "root_partuuid": "'
                        + root
                        + '", "slot": "'
                        + slot
                        + '"}',
                        "FLOWERLY_GENERATION_USER_MARKER_SHA256=" + marker_of[lineage],
                    ]
                    lines += (
                        [
                            "FLOWERLY_GENERATION_IPC_VERIFIED",
                            "FLOWERLY_GENERATION_BOOT_BLESSED",
                        ]
                        if expect == "BLESSED"
                        else ["injected unhealthy Generation boot"]
                    )
                name = label + ".serial.log"
                (folder / name).write_text("\n".join(lines) + "\n")
                checksums[name] = kvm.sha_file(folder / name)
                boots.append(
                    {
                        "label": label,
                        "slot": slot,
                        "expect": expect,
                        "lineage": lineage,
                        "serial": name,
                        "marker_sha256": None
                        if expect == "NO_WITNESS"
                        else marker_of[lineage],
                        "observed_health": expect,
                        "seconds_to_observed": None if expect == "NO_WITNESS" else 9.5,
                        "power_loss": "QEMU_SIGKILL",
                        "kvm_fd_verified": True,
                        "qemu_cmdline_sha256": "c" * 64,
                    }
                )
            (folder / "serial-sha256.json").write_text(json.dumps(checksums))
            passing = set(kvm.SCENARIOS)
            evidence = {
                "schema": "flowerly-generation-kvm-sequence/v1",
                "source_sha": "a" * 40,
                "disk_sha256_initial": H,
                "ovmf_code_sha256": H,
                "ovmf_vars_template_sha256": H,
                "manifest_sha256": H,
                "root_a_partuuid": root_a,
                "root_b_partuuid": root_b,
                "release_acceptance": False,
                "sequence_verified": True,
                "status": "SEQUENCE_OBSERVED_S7_INCOMPLETE",
                "boot_tries": tries,
                "scenarios": {name: "PASS" for name in passing},
                "host_checks": {
                    name: {"ok": True}
                    for name in (
                        "stage-complete",
                        "stage-interrupted",
                        "tampered-b-refused",
                    )
                },
                "measurements": {
                    "stage_seconds": 12.0,
                    "slot_bytes": {"root-a": 1, "root-b": 1},
                },
                "boots": boots,
            }
            report = audit.audit(evidence, folder)
            self.assertEqual(report["boots"], len(plan))
            self.assertEqual(set(report["scenarios_pass"]), passing)

            def refuses(mutator):
                changed = copy.deepcopy(evidence)
                mutator(changed)
                with self.assertRaises(ValueError):
                    audit.audit(changed, folder)

            def boot(report, label):
                return next(b for b in report["boots"] if b["label"] == label)

            refuses(lambda r: boot(r, "healthy-b-2").update(marker_sha256="b" * 64))
            refuses(lambda r: boot(r, "unhealthy-b-1").update(slot="root-a"))
            refuses(lambda r: boot(r, "boot-a").update(serial="healthy-b-1.serial.log"))
            refuses(lambda r: r.update(release_acceptance=True))
            refuses(lambda r: boot(r, "boot-a").update(kvm_fd_verified=False))
            refuses(lambda r: boot(r, "rollback-a").update(qemu_cmdline_sha256="x"))
            refuses(lambda r: r["scenarios"].pop("data-survives"))
            refuses(lambda r: r["boots"].pop(2))
            refuses(lambda r: r["boots"].reverse())
            refuses(lambda r: boot(r, "stage-interrupted-a").update(lineage="main"))
            # A scenario may only be PASS with its evidence, and observed
            # evidence may not be left as NOT_RUN either.
            refuses(lambda r: r["host_checks"]["stage-complete"].update(ok=False))
            refuses(lambda r: r["measurements"].pop("stage_seconds"))
            refuses(lambda r: r["scenarios"].update({"rollback-to-a": "NOT_RUN"}))
            refuses(lambda r: r.update(boot_tries=3))
            refuses(lambda r: r["host_checks"].update(bogus={"ok": True}))
            for label, text, replacement in (
                ("healthy-b-1", "FLOWERLY_GENERATION_BOOT_BLESSED\n", ""),
                ("unhealthy-b-2", "injected unhealthy Generation boot\n", ""),
                ("damaged-b-1", "initrd=\\flowerly\\b", "initrd=\\flowerly\\a"),
                ("rollback-a", "initrd=\\flowerly\\a", "initrd=\\flowerly\\b"),
            ):
                log = folder / (label + ".serial.log")
                original = log.read_text()
                self.assertIn(text, original)
                log.write_text(original.replace(text, replacement))
                with self.assertRaises(ValueError, msg=label):
                    audit.audit(evidence, folder)
                log.write_text(original)
            damaged = folder / "damaged-b-2.serial.log"
            original = damaged.read_text()
            damaged.write_text(original + "FLOWERLY_GENERATION_ROOT_VERIFIED\n")
            with self.assertRaises(ValueError):
                audit.audit(evidence, folder)
            damaged.write_text(original)
            log = folder / "healthy-b-1.serial.log"
            text = log.read_text()
            own = "00000004-1111-1111-1111-111111111111"
            self.assertIn(own, text)
            # Same kernel boot ID as the first boot: a replayed log.
            log.write_text(text.replace(own, "00000000-1111-1111-1111-111111111111"))
            with self.assertRaises(ValueError):
                audit.audit(evidence, folder)
            log.write_text(text)
            # A symlinked serial file is never accepted even when its
            # content and digest would otherwise match.
            log.rename(folder / "real-boot-serial.log")
            log.symlink_to(folder / "real-boot-serial.log")
            with self.assertRaises(ValueError):
                audit.audit(evidence, folder)

    def test_only_esp_and_user_data_are_carried_between_guests(self):
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            pristine = folder / "pristine.img"
            booted = folder / "after-boot-a.img"
            dest = folder / "reconciled.img"
            a = b"".join(bytes([i]) * MIB for i in range(7))
            b = bytearray(a)
            b[MIB + 4096] = 200  # EFI boot-count rename metadata
            b[5 * MIB + 512] = 201  # persistent user-data marker
            b[2 * MIB + 2048] = 202  # incidental rootfs ext4 write
            pristine.write_bytes(a)
            booted.write_bytes(b)
            manifest = {
                "partitions": [
                    {
                        "name": name,
                        "offset_bytes": i * MIB,
                        "size_bytes": (2 if name == "root-b" else 1) * MIB,
                    }
                    for i, name in (
                        (1, "esp"),
                        (2, "root-a"),
                        (3, "root-b"),
                        (5, "data"),
                    )
                ]
            }
            for part in manifest["partitions"]:
                start, size = part["offset_bytes"], part["size_bytes"]
                part["sha256"] = hashlib.sha256(a[start : start + size]).hexdigest()
            roots = kvm.verify_immutable_roots(pristine, manifest)
            self.assertEqual(set(roots), {"root-a", "root-b"})
            with self.assertRaisesRegex(ValueError, "guest changed pinned root-a"):
                kvm.verify_immutable_roots(booted, manifest)
            # Only the two explicitly mutable sections are copied.
            hashes = kvm.reconcile_mutable_partitions(pristine, booted, dest, manifest)
            result = dest.read_bytes()
            self.assertEqual(result[MIB : 2 * MIB], bytes(b[MIB : 2 * MIB]))
            self.assertEqual(result[5 * MIB : 6 * MIB], bytes(b[5 * MIB : 6 * MIB]))
            self.assertEqual(result[2 * MIB : 5 * MIB], a[2 * MIB : 5 * MIB])
            self.assertEqual(
                hashes["data"], hashlib.sha256(b[5 * MIB : 6 * MIB]).hexdigest()
            )

    def test_process_argv_never_fakes_kvm_evidence(self):
        import os

        with self.assertRaises(ValueError):
            kvm.verify_running_qemu(
                os.getpid(), ["qemu-system-x86_64", "-machine", "q35,accel=tcg"]
            )

    def test_stale_commit_fails_before_touching_disk_or_kvm(self):
        args = SimpleNamespace(approved_sha="f" * 40)
        with self.assertRaises(ValueError):
            kvm.validate_source(args, actual_execution=False)


if __name__ == "__main__":
    unittest.main()
