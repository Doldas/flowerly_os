#!/usr/bin/env python3
"""Offline 0.0.5 regression for issue #321."""
import importlib.util
from pathlib import Path
spec=importlib.util.spec_from_file_location("generation_005_01",Path(__file__).with_name("generation-005-01-artifact.py"))
mod=importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
import hashlib
import tempfile
import unittest
from pathlib import Path

class Tests(unittest.TestCase):
    def test_match_and_tamper(self):
        with tempfile.TemporaryDirectory() as temp:
            p = Path(temp) / "kernel"
            p.write_bytes(b"kernel")
            h = hashlib.sha256(b"kernel").hexdigest()
            self.assertEqual(mod.verify_artifact(p, h)["bytes"], 6)
            p.write_bytes(b"tampered")
            with self.assertRaises(ValueError):
                mod.verify_artifact(p, h)

    def test_invalid_hash_and_symlink(self):
        with tempfile.TemporaryDirectory() as temp:
            p = Path(temp) / "kernel"
            p.write_bytes(b"kernel")
            link = Path(temp) / "link"
            link.symlink_to(p)
            for path, digest in [(p, "0"*63), (p, "G"*64), (link, hashlib.sha256(b"kernel").hexdigest())]:
                with self.subTest(path=path, digest=digest):
                    with self.assertRaises(ValueError):
                        mod.verify_artifact(path, digest)

if __name__=="__main__":
    unittest.main()
