#!/usr/bin/env python3
"""Offline 0.0.5 regression for issue #325."""
import importlib.util
from pathlib import Path
import unittest
spec=importlib.util.spec_from_file_location("generation_005_05",Path(__file__).with_name("generation-005-05-efi.py"))
mod=importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
class Tests(unittest.TestCase):
    def test_accepted(self):
        self.assertEqual(mod.selected_entry("flowerly-a+3.conf")["slot"],"root-a")
        self.assertEqual(mod.selected_entry("flowerly-b")["slot"],"root-b")
    def test_rejected(self):
        for raw in ("../flowerly-a.conf","flowerly-a+0.conf","flowerly-a.conf\n",
                    "flowerly-a.conf\x00","flowerly-c.conf","flowerly-b+999.conf",42):
            with self.subTest(raw=raw),self.assertRaises(ValueError):
                mod.selected_entry(raw)

if __name__=="__main__":
    unittest.main()
