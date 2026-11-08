#!/usr/bin/env python3
"""Offline 0.0.5 regression for issue #328."""
import importlib.util
from pathlib import Path
import json
import tempfile
import unittest
spec=importlib.util.spec_from_file_location("generation_005_08",Path(__file__).with_name("generation-005-08-replay.py"))
mod=importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

def row(n,slot="root-a"):
    return {"schema":"flowerly-boot-witness/v1","boot_id":f"boot-{n}",
            "sequence":n,"slot":slot,"manifest_sha256":"a"*64}
class Tests(unittest.TestCase):
    def test_ordered(self):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d)/"witness"
            p.write_text(json.dumps(row(1))+"\n"+json.dumps(row(2,"root-b"))+"\n")
            self.assertEqual(mod.replay_witness(p)["last_slot"],"root-b")
    def test_missing_tail_and_duplicates(self):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d)/"witness"
            for data in (json.dumps(row(1)),json.dumps(row(1))+"\n"+json.dumps(row(1))+"\n",
                         json.dumps(row(2))+"\n"+json.dumps(row(1))+"\n","{bad}\n"):
                p.write_text(data)
                with self.subTest(data=data),self.assertRaises(ValueError):
                    mod.replay_witness(p)

if __name__=="__main__":
    unittest.main()
