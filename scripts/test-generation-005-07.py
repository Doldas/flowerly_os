#!/usr/bin/env python3
"""Offline 0.0.5 regression for issue #327."""
import importlib.util
from pathlib import Path
spec=importlib.util.spec_from_file_location("generation_005_07",Path(__file__).with_name("generation-005-07-witness.py"))
mod=importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
import json
import tempfile
import unittest
from pathlib import Path

def fixture(seq=1):
    return {"schema":"flowerly-boot-witness/v1","boot_id":"boot-a","slot":"root-a",
            "manifest_sha256":"a"*64,"sequence":seq}
class Tests(unittest.TestCase):
    def test_append_and_refuse_invalid(self):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d)/"witness.jsonl"
            self.assertGreater(mod.append_witness(p,fixture())["appended_bytes"],0)
            mod.append_witness(p,fixture(2))
            self.assertEqual(len(p.read_text().splitlines()),2)
            for value in ({"sequence":True},{"slot":"root-c"},{"manifest_sha256":"token"}):
                record=fixture();record.update(value)
                with self.assertRaises(ValueError):
                    mod.append_witness(p,record)
            self.assertEqual(len(p.read_text().splitlines()),2)
    def test_symlink_refusal(self):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d)/"real";p.write_text("secret")
            link=Path(d)/"link";link.symlink_to(p)
            with self.assertRaises(ValueError):
                mod.append_witness(link,fixture())
            self.assertEqual(p.read_text(),"secret")

if __name__=="__main__":
    unittest.main()
