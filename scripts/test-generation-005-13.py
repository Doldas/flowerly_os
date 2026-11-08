#!/usr/bin/env python3
"""Offline 0.0.5 regression for issue #333."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
spec=importlib.util.spec_from_file_location("generation_005_13",Path(__file__).with_name("generation-005-13-data.py"))
mod=importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
class Tests(unittest.TestCase):
    def test_preserved_and_changed(self):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d)/"data"
            p.write_bytes(b"user-data")
            before=mod.marker_digest(p)
            self.assertEqual(mod.verify_persistence(before,mod.marker_digest(p))["sha256"],before)
            p.write_bytes(b"modified")
            with self.assertRaises(ValueError):
                mod.verify_persistence(before,mod.marker_digest(p))
    def test_symlink_refusal(self):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d)/"real"
            p.write_bytes(b"user")
            link=Path(d)/"link"
            link.symlink_to(p)
            with self.assertRaises(ValueError):
                mod.marker_digest(link)

if __name__=="__main__":
    unittest.main()
