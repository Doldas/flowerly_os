#!/usr/bin/env python3
import hashlib
import importlib.util
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "slots", Path(__file__).with_name("verify-ab-slot-layout.py")
)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
MIB = 1048576


class ABSlotTests(unittest.TestCase):
    def test_verified_layout_and_tampering(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "disk.img"
            payload = b"".join(bytes([number]) * MIB for number in range(5))
            path.write_bytes(payload)
            parts = []
            for number, name in enumerate(("esp", "root-a", "root-b", "data"), start=1):
                parts.append(
                    {
                        "name": name,
                        "offset_bytes": number * MIB,
                        "size_bytes": MIB,
                        "partuuid": f"part-{number}",
                        "sha256": hashlib.sha256(bytes([number]) * MIB).hexdigest(),
                    }
                )
            manifest = {
                "schema": "flowerly-ab-disk/v1",
                "disk": {
                    "size_bytes": len(payload),
                    "sha256": hashlib.sha256(payload).hexdigest(),
                },
                "partitions": parts,
            }
            self.assertEqual(mod.verify(path, manifest)["partitions_verified"], 4)
            tampered = bytearray(payload)
            tampered[0] ^= 1
            path.write_bytes(tampered)
            with self.assertRaises(ValueError):
                mod.verify(path, manifest)
            path.write_bytes(payload)
            parts[2]["partuuid"] = parts[1]["partuuid"]
            with self.assertRaises(ValueError):
                mod.verify(path, manifest)
            parts[2]["partuuid"] = "part-3"
            parts[2]["offset_bytes"] = parts[1]["offset_bytes"]
            with self.assertRaises(ValueError):
                mod.verify(path, manifest)
            parts[2]["offset_bytes"] = 3 * MIB
            parts[2]["sha256"] = "0" * 64
            with self.assertRaises(ValueError):
                mod.verify(path, manifest)


if __name__ == "__main__":
    unittest.main()
