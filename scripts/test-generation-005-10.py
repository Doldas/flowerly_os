#!/usr/bin/env python3
"""Offline 0.0.5 regression for issue #330."""
import importlib.util
from pathlib import Path
spec=importlib.util.spec_from_file_location("generation_005_10",Path(__file__).with_name("generation-005-10-stage.py"))
mod=importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
import hashlib
import tempfile
import unittest
from pathlib import Path
class Tests(unittest.TestCase):
    def test_valid_and_refusal(self):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d)/"slot.img";p.write_bytes(b"X"*1048576)
            h=hashlib.sha256(p.read_bytes()).hexdigest()
            self.assertEqual(mod.preflight_stage("root-a","root-b",p,h,1048576)["target"],"root-b")
            for active,target,hash_ in (("root-a","root-a",h),("root-a","root-b","0"*64),
                                         ("evil","root-b",h)):
                with self.assertRaises(ValueError):
                    mod.preflight_stage(active,target,p,hash_,1048576)
            link=Path(d)/"symlink";link.symlink_to(p)
            with self.assertRaises(ValueError):
                mod.preflight_stage("root-a","root-b",link,h,1048576)

if __name__=="__main__":
    unittest.main()
