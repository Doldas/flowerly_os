#!/usr/bin/env python3
"""Offline 0.0.5 regression for issue #332."""
import importlib.util
from pathlib import Path
import unittest
spec=importlib.util.spec_from_file_location("generation_005_12",Path(__file__).with_name("generation-005-12-status.py"))
mod=importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
H="a"*64
H="a"*64
B="b"*64
P={"root-a":H,"root-b":B}
class Tests(unittest.TestCase):
    def test_verified_activation_and_fallback(self):
        self.assertEqual(mod.reconcile("root-a","root-b","root-b",True,P,B),"ACTIVE")
        self.assertEqual(mod.reconcile("root-a","root-b","root-a",True,P,H),"FELL_BACK")
    def test_never_claim_after_uncertainty(self):
        for booted,healthy,observed in (("root-b",False,B),("root-b","true",B),
                                       (None,True,B),("root-b",True,H)):
            self.assertEqual(mod.reconcile("root-a","root-b",booted,healthy,P,observed),"INCONCLUSIVE")
    def test_refuse_ambiguous_pending_or_missing_identity(self):
        with self.assertRaises(ValueError):
            mod.reconcile("root-a","root-a","root-a",True,P,H)
        with self.assertRaises(ValueError):
            mod.reconcile("root-a","root-b","root-b",True,{"root-b":B},B)

if __name__=="__main__":
    unittest.main()
