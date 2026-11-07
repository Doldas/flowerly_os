#!/usr/bin/env python3
"""Offline 0.0.5 regression for issue #324."""
import importlib.util
from pathlib import Path
spec=importlib.util.spec_from_file_location("generation_005_04",Path(__file__).with_name("generation-005-04-loader.py"))
mod=importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
import unittest
class Tests(unittest.TestCase):
    def test_counting_and_blessed(self):
        self.assertEqual(mod.parse_entry("flowerly-b+3.conf"),{"slot":"root-b","remaining_tries":3})
        self.assertIsNone(mod.parse_entry("flowerly-a.conf")["remaining_tries"])
    def test_reject_traversal_and_noncanonical(self):
        for name in ("../flowerly-a.conf","flowerly-a+0.conf","flowerly-b+100.conf",
                     "flowerly-c+1.conf","flowerly-a+01.conf","flowerly-a.conf.bak",""):
            with self.subTest(name=name),self.assertRaises(ValueError):
                mod.parse_entry(name)

if __name__=="__main__":
    unittest.main()
