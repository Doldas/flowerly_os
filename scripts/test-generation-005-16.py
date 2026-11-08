#!/usr/bin/env python3
"""Offline 0.0.5 regression for issue #336."""
import importlib.util
from pathlib import Path
import unittest
spec=importlib.util.spec_from_file_location("generation_005_16",Path(__file__).with_name("generation-005-16-packages.py"))
mod=importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
def package(name,version="1.0"):
    return {"name":name,"version":version,"architecture":"x86_64",
            "sha256":"a"*64,"repository":"core"}
class Tests(unittest.TestCase):
    def test_order_independent_identity(self):
        first=mod.package_set_identity([package("bash"),package("linux")])
        second=mod.package_set_identity([package("linux"),package("bash")])
        self.assertEqual(first["sha256"],second["sha256"])
    def test_refuse_unpinned_duplicates_and_bad_hash(self):
        for packages in ([package("bash"),package("bash")],[package("bash",version="latest")],
                         [dict(package("bash"),sha256="x"*64)],[]):
            with self.subTest(packages=packages),self.assertRaises(ValueError):
                mod.package_set_identity(packages)

if __name__=="__main__":
    unittest.main()
