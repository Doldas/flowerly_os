#!/usr/bin/env python3
"""Offline 0.0.5 regression for issue #334."""
import importlib.util
from pathlib import Path
import unittest
spec=importlib.util.spec_from_file_location("generation_005_14",Path(__file__).with_name("generation-005-14-evidence.py"))
mod=importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
def report():
    return {"schema":"flowerly-generation-kvm/v1","source_commit":"a"*40,
            "qemu_version":"QEMU emulator version 9.0","ovmf_sha256":"b"*64,"disk_sha256":"c"*64,
            "booted_root_partuuid":"12345678-1234-1234-1234-1234567890ab",
            "boot_witness":{"ipc_healthy":True,"root_verified":True},
            "scenarios":{scenario:"PASS" for scenario in ("stage-complete","stage-interrupted",
               "arm-before-rename","arm-after-rename","boot-before-bless",
               "unhealthy-b-fallback","data-survives","tampered-b-refused")}}
class Tests(unittest.TestCase):
    def test_structural_evidence(self):
        self.assertEqual(mod.validate_evidence(report())["status"],"STRUCTURALLY_COMPLETE_ONLY")
    def test_not_run_not_pass(self):
        r=report()
        r["scenarios"]["boot-before-bless"]="NOT_RUN"
        self.assertEqual(mod.validate_evidence(r)["status"],"INCOMPLETE")
    def test_missing_scenario_never_counts_as_complete(self):
        r=report()
        del r["scenarios"]["boot-before-bless"]
        with self.assertRaises(ValueError):
            mod.validate_evidence(r)

    def test_reject_missing_or_false_claim(self):
        for name,bad in (("source_commit","none"),("ovmf_sha256","x"),
                         ("qemu_version",""),("boot_witness",{"ipc_healthy":"true","root_verified":True})):
            r=report()
            r[name]=bad
            with self.subTest(name=name),self.assertRaises(ValueError):
                mod.validate_evidence(r)

if __name__=="__main__":
    unittest.main()
