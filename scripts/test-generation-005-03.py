#!/usr/bin/env python3
"""Offline 0.0.5 regression for issue #323."""
import importlib.util
from pathlib import Path
spec=importlib.util.spec_from_file_location("generation_005_03",Path(__file__).with_name("generation-005-03-root.py"))
mod=importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
import unittest
A="12345678-1234-1234-1234-1234567890ab"
B="12345678-1234-1234-1234-1234567890cd"
M={"partitions":[{"name":"root-a","partuuid":A},{"name":"root-b","partuuid":B}]}
class Tests(unittest.TestCase):
    def test_known_root(self):
        self.assertEqual(mod.booted_slot("console=ttyS0 root=PARTUUID="+A,M),"root-a")
    def test_unknown_or_ambiguous_root(self):
        for text in ("root=/dev/sda2","root=PARTUUID="+A+" root=PARTUUID="+B,
                     "root=PARTUUID=bad","root=PARTUUID="+("0"*8+"-"+"0"*4+"-"+"0"*4+"-"+"0"*4+"-"+"0"*12),""):
            with self.subTest(text=text),self.assertRaises(ValueError):
                mod.booted_slot(text,M)

if __name__=="__main__":
    unittest.main()
