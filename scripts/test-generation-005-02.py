#!/usr/bin/env python3
"""Offline 0.0.5 regression for issue #322."""
import importlib.util
from pathlib import Path
import unittest
spec=importlib.util.spec_from_file_location("generation_005_02",Path(__file__).with_name("generation-005-02-partition.py"))
mod=importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

def fixture():
    return {"schema":"flowerly-ab-disk/v1","disk":{"size_bytes":6*1048576},
            "partitions":[{"name":name,"offset_bytes":(i+1)*1048576,
                           "size_bytes":1048576,"partuuid":f"id-{i}",
                           "sha256":"a"*64} for i,name in enumerate(mod.NAMES)]}

class Tests(unittest.TestCase):
    def test_valid(self):
        self.assertEqual(mod.validate_layout(fixture())["partitions"],4)

    def test_reject_bad_geometry_and_aliases(self):
        for mutation in ("overlap","bool","duplicate","wrong-order","bad-hash","out-of-range"):
            f=fixture()
            if mutation=="overlap":
                f["partitions"][1]["offset_bytes"]=1048576
            if mutation=="bool":
                f["partitions"][0]["size_bytes"]=True
            if mutation=="duplicate":
                f["partitions"][1]["partuuid"]="id-0"
            if mutation=="wrong-order":
                f["partitions"].reverse()
            if mutation=="bad-hash":
                f["partitions"][0]["sha256"]="bad"
            if mutation=="out-of-range":
                f["disk"]["size_bytes"]=2*1048576
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                mod.validate_layout(f)

if __name__=="__main__":
    unittest.main()
