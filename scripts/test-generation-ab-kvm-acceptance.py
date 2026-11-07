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
audit_spec = importlib.util.spec_from_file_location(
    "audit", HERE / "generation-ab-evidence-audit.py")
audit = importlib.util.module_from_spec(audit_spec)
audit_spec.loader.exec_module(audit)
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

    def test_end_to_end_serial_audit_and_tampered_evidence(self):
        import copy
        import json
        ids = [
            "11111111-1111-1111-1111-111111111111",
            "22222222-2222-2222-2222-222222222222",
            "33333333-3333-3333-3333-333333333333",
            "44444444-4444-4444-4444-444444444444",
            "55555555-5555-5555-5555-555555555555",
        ]
        root_a = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
        root_b = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
        names = ["boot-a.serial.log", "unhealthy-b-1.serial.log",
                 "unhealthy-b-2.serial.log", "unhealthy-b-3.serial.log",
                 "auto-fallback-a.serial.log"]
        with tempfile.TemporaryDirectory() as d:
            folder = Path(d)
            boots = []
            checksums = {}
            for i, name in enumerate(names):
                slot = "root-a" if i in (0, 4) else "root-b"
                root = root_a if slot == "root-a" else root_b
                blessed = slot == "root-a"
                lines = [
                    "FLOWERLY_GENERATION_ROOT_VERIFIED",
                    '{"boot_id": "' + ids[i] + '", "root_partuuid": "' + root +
                    '", "slot": "' + slot + '"}',
                    "FLOWERLY_GENERATION_USER_MARKER_SHA256=" + H,
                ]
                lines += (["FLOWERLY_GENERATION_IPC_VERIFIED",
                           "FLOWERLY_GENERATION_BOOT_BLESSED"] if blessed
                          else ["injected unhealthy Generation boot"])
                log = folder / name
                log.write_text("\n".join(lines) + "\n")
                checksums[name] = kvm.sha_file(log)
                boots.append({"slot": slot, "serial": name,
                              "marker_sha256": H,
                              "observed_health": "BLESSED" if blessed else "REJECTED",
                              "power_loss": "QEMU_SIGKILL"})
            (folder / "serial-sha256.json").write_text(json.dumps(checksums))
            evidence = {
                "schema": "flowerly-generation-kvm-sequence/v1",
                "source_sha": "a"*40, "disk_sha256_initial": H,
                "ovmf_code_sha256": H, "ovmf_vars_template_sha256": H,
                "manifest_sha256": H,
                "root_a_partuuid": root_a, "root_b_partuuid": root_b,
                "release_acceptance": False,
                "sequence_verified": True,
                "status": "SEQUENCE_OBSERVED_S7_INCOMPLETE",
                "boot_tries": 3,
                "scenarios": {name: ("PASS" if name in {
                    "boot-before-bless", "unhealthy-b-fallback", "data-survives"
                } else "NOT_RUN") for name in kvm.SCENARIOS},
                "boots": boots,
            }
            self.assertEqual(
                audit.audit(evidence, folder)["boots"], 5)

            def refuses(mutator):
                changed = copy.deepcopy(evidence)
                mutator(changed)
                with self.assertRaises(ValueError):
                    audit.audit(changed, folder)

            refuses(lambda r: r["boots"][2].update(marker_sha256="b"*64))
            refuses(lambda r: r["boots"][1].update(slot="root-a"))
            refuses(lambda r: r["boots"][0].update(serial="unhealthy-b-1.serial.log"))
            refuses(lambda r: r.update(boot_tries=2))
            refuses(lambda r: r.update(release_acceptance=True))
            refuses(lambda r: r["scenarios"].update({"stage-interrupted": "PASS"}))
            refuses(lambda r: r["scenarios"].pop("data-survives"))
            refuses(lambda r: r["boots"].pop(2))
            refuses(lambda r: r["boots"][2].update(observed_health="BLESSED"))
            log = folder / names[2]
            log.write_text(log.read_text().replace(ids[2], ids[1]))
            with self.assertRaises(ValueError):
                audit.audit(evidence, folder)
            log.write_text(log.read_text().replace(ids[1], ids[2]))
            log.write_text(log.read_text() + "FLOWERLY_GENERATION_BOOT_BLESSED\n")
            with self.assertRaises(ValueError):
                audit.audit(evidence, folder)
            log.write_text(log.read_text().replace(
                "FLOWERLY_GENERATION_BOOT_BLESSED\n", ""))
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
