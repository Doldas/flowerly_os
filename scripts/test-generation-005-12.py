#!/usr/bin/env python3
"""Offline 0.0.5 regression for issue #332."""
import importlib.util
from pathlib import Path
spec=importlib.util.spec_from_file_location("generation_005_12",Path(__file__).with_name("generation-005-12-status.py"))
mod=importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
import unittest
H="a"*64
class Tests(unittest.TestCase):
    def test_verified_activation_and_fallback(self):
        self.assertEqual(mod.reconcile("root-a","root-b","root-b",True,H,H),"ACTIVE")
        self.assertEqual(mod.reconcile("root-a","root-b","root-a",True,H,H),"FELL_BACK")
    def test_never_claim_after_uncertainty(self):
        for booted,healthy,observed in (("root-b",False,H),("root-b","true",H),
                                       (None,True,H),("root-b",True,"b"*64)):
            self.assertEqual(mod.reconcile("root-a","root-b",booted,healthy,H,observed),"INCONCLUSIVE")
    def test_refuse_ambiguous_pending(self):
        with self.assertRaises(ValueError):
            mod.reconcile("root-a","root-a","root-a",True,H,H)

if __name__=="__main__":
    unittest.main()
