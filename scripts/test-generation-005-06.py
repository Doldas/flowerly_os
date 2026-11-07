#!/usr/bin/env python3
"""Offline 0.0.5 regression for issue #326."""
import importlib.util
from pathlib import Path
spec=importlib.util.spec_from_file_location("generation_005_06",Path(__file__).with_name("generation-005-06-identity.py"))
mod=importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
import unittest
S="a"*64
C="b"*40
U="12345678-1234-1234-1234-1234567890ab"
def valid():
    return {"schema":"flowerly-generation-root/v1","manifest_sha256":S,"source_commit":C,
            "root_partuuid":U,"build_id":"fixture-1"}
class Tests(unittest.TestCase):
    def test_verified(self):
        self.assertEqual(mod.verify_identity(valid(),manifest_sha=S,source_commit=C,booted_partuuid=U)["root_identity"],"matched_offline")
    def test_mismatches(self):
        for field,value in (("manifest_sha256","x"*64),("source_commit","deadbeef"),
                            ("root_partuuid","bad"),("build_id","../etc/passwd"),("schema","v2")):
            o=valid();o[field]=value
            with self.subTest(field=field),self.assertRaises(ValueError):
                mod.verify_identity(o,manifest_sha=S,source_commit=C,booted_partuuid=U)

if __name__=="__main__":
    unittest.main()
