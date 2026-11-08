#!/usr/bin/env python3
"""Real file writes and injected interruption for inactive A/B slot staging."""

import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location(
    "stage", HERE / "generation-slot-stage-image.py"
)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
MIB = 1048576
A = "11111111-1111-1111-1111-111111111111"
B = "22222222-2222-2222-2222-222222222222"


def digest(data):
    return hashlib.sha256(data).hexdigest()


def fixture(root):
    # Layout: alignment gap, ESP, root A, root B (2 MiB), data, backup gap.
    blocks = [bytes([i]) * MIB for i in range(7)]
    data = b"".join(blocks)
    disk = root / "disk.img"
    disk.write_bytes(data)
    parts = [
        ("esp", 1, 1, "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee"),
        ("root-a", 2, 1, A),
        ("root-b", 3, 2, B),
        ("data", 5, 1, "dddddddd-dddd-dddd-dddd-dddddddddddd"),
    ]
    manifest = {
        "schema": "flowerly-ab-disk/v1",
        "disk": {"size_bytes": len(data), "sha256": digest(data)},
        "partitions": [
            {
                "name": name,
                "offset_bytes": start * MIB,
                "size_bytes": n * MIB,
                "sha256": digest(data[start * MIB : (start + n) * MIB]),
                "partuuid": uid,
            }
            for name, start, n, uid in parts
        ],
    }
    path = root / "disk.manifest.json"
    path.write_text(json.dumps(manifest))
    candidate = root / "candidate.img"
    candidate.write_bytes(b"X" * MIB + b"Y" * MIB)
    return disk, path, candidate


class StageTests(unittest.TestCase):
    def test_stage_writes_only_inactive_slot_then_verifies(self):
        with tempfile.TemporaryDirectory() as d:
            disk, manifest, candidate = fixture(Path(d))
            before = disk.read_bytes()
            image = candidate.read_bytes()
            result = module.stage(disk, manifest, candidate, digest(image), A, "root-b")
            self.assertEqual(result["status"], "STAGED_NOT_ARMED")
            after = disk.read_bytes()
            self.assertEqual(
                after[: 3 * MIB], before[: 3 * MIB], "ESP and active root changed"
            )
            self.assertEqual(after[3 * MIB : 5 * MIB], image)
            self.assertEqual(
                after[5 * MIB :], before[5 * MIB :], "user-data partition changed"
            )
            spec = importlib.util.spec_from_file_location(
                "abcheck", HERE / "verify-ab-slot-layout.py"
            )
            checker = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(checker)
            self.assertEqual(
                checker.verify(disk, json.loads(manifest.read_text()))[
                    "partitions_verified"
                ],
                4,
            )

    def test_refuse_active_wrong_hash_and_symlink(self):
        with tempfile.TemporaryDirectory() as d:
            disk, manifest, candidate = fixture(Path(d))
            before = disk.read_bytes()
            sha = digest(candidate.read_bytes())
            for active, target, expected in (
                (B, "root-b", sha),
                (A, "root-b", "a" * 64),
                ("unknown", "root-b", sha),
            ):
                with self.assertRaises(ValueError):
                    module.stage(disk, manifest, candidate, expected, active, target)
            alias = Path(d) / "candidate-link"
            alias.symlink_to(candidate)
            with self.assertRaises(ValueError):
                module.stage(disk, manifest, alias, sha, A, "root-b")
            self.assertEqual(disk.read_bytes(), before)

    def test_interrupted_write_keeps_active_esp_and_data(self):
        with tempfile.TemporaryDirectory() as d:
            disk, manifest, candidate = fixture(Path(d))
            old_disk = disk.read_bytes()
            old_manifest = manifest.read_bytes()
            with self.assertRaisesRegex(RuntimeError, "power loss"):
                module.stage(
                    disk,
                    manifest,
                    candidate,
                    digest(candidate.read_bytes()),
                    A,
                    "root-b",
                    inject_after_chunks=1,
                )
            new_disk = disk.read_bytes()
            self.assertEqual(new_disk[: 3 * MIB], old_disk[: 3 * MIB])
            self.assertEqual(new_disk[5 * MIB :], old_disk[5 * MIB :])
            self.assertNotEqual(new_disk, old_disk)
            self.assertEqual(
                manifest.read_bytes(),
                old_manifest,
                "incomplete staging must not publish new manifest",
            )
            with self.assertRaises(ValueError):
                module.stage(
                    disk,
                    manifest,
                    candidate,
                    digest(candidate.read_bytes()),
                    A,
                    "root-b",
                )


if __name__ == "__main__":
    unittest.main()
