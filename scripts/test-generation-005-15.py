#!/usr/bin/env python3
"""Offline 0.0.5 regression for issue #335."""

import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location(
    "generation_005_15", Path(__file__).with_name("generation-005-15-faults.py")
)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)


class Tests(unittest.TestCase):
    def test_all_eight_must_pass(self):
        done = {k: "PASS" for k in mod.SCENARIOS}
        self.assertEqual(mod.validate_scenarios(done)["gate"], "PASS")
        done["arm-after-rename"] = "NOT_RUN"
        self.assertEqual(mod.validate_scenarios(done)["gate"], "BLOCKED")
        self.assertEqual(
            mod.validate_scenarios(done)["unverified"], ["arm-after-rename"]
        )

    def test_incomplete_and_forged_result(self):
        done = {k: "PASS" for k in mod.SCENARIOS}
        del done["boot-before-bless"]
        with self.assertRaises(ValueError):
            mod.validate_scenarios(done)
        done["boot-before-bless"] = True
        with self.assertRaises(ValueError):
            mod.validate_scenarios(done)


if __name__ == "__main__":
    unittest.main()
