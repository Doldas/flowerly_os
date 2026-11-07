#!/usr/bin/env python3
"""Offline 0.0.5 regression for issue #331."""
import importlib.util
from pathlib import Path
spec=importlib.util.spec_from_file_location("generation_005_11",Path(__file__).with_name("generation-005-11-arm.py"))
mod=importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
import unittest
H="a"*64
class Tests(unittest.TestCase):
    def test_explicit_arm(self):
        self.assertEqual(mod.arm_request("root-a","root-b",3,H,H,H)["entry"],"flowerly-b+3.conf")
    def test_refuse_unsafe(self):
        for args in (("root-a","root-a",3,H,H,H),("root-a","root-b",0,H,H,H),
                     ("root-a","root-b",True,H,H,H),("root-a","root-b",3,H,H,"b"*64),
                     ("root-b","root-a",3,H,"invalid",H)):
            with self.subTest(args=args),self.assertRaises(ValueError):
                mod.arm_request(*args)

if __name__=="__main__":
    unittest.main()
