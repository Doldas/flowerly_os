#!/usr/bin/env python3
import importlib.util
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "unit_policy", Path(__file__).with_name("coreonly-unit-policy.py")
)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)


class CoreOnlyTests(unittest.TestCase):
    def test_coreonly_refuses_optional_compute_without_disabling_core(self):
        self.assertEqual(mod.check_plan("core_only", ["flowerlyd.service"]), [])
        self.assertEqual(
            mod.check_plan(
                "core_only",
                ["flowerlyd.service", "evolutiond.service", "providerd.service"],
            ),
            ["evolutiond.service", "providerd.service"],
        )

    def test_other_modes_cannot_implicitly_authorize_network(self):
        for mode in ("standalone", "cluster"):
            self.assertEqual(
                mod.check_plan(mode, ["providerd.service"]), ["providerd.service"]
            )
        with self.assertRaises(ValueError):
            mod.allowed_units("CORE_ONLY")


if __name__ == "__main__":
    unittest.main()
