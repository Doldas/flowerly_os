#!/usr/bin/env python3
"""Offline 0.0.5 regression for issue #329."""

import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location(
    "generation_005_09", Path(__file__).with_name("generation-005-09-health.py")
)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
OK = {
    "elapsed_seconds": 2,
    "ipc_healthy": True,
    "boot_id": "boot1",
    "root_verified": True,
}


class Tests(unittest.TestCase):
    def test_explicit_pass(self):
        self.assertEqual(mod.health_verdict(OK)["verdict"], "PASS")

    def test_timeout_and_untruthful_flags(self):
        for delta in (
            {"elapsed_seconds": 40},
            {"ipc_healthy": "true"},
            {"root_verified": False},
            {"elapsed_seconds": float("nan")},
            {"boot_id": ""},
        ):
            evidence = dict(OK, **delta)
            with self.subTest(delta=delta):
                self.assertEqual(mod.health_verdict(evidence)["verdict"], "FAIL")

    def test_bad_timeout(self):
        for t in (0, False, 301, -1):
            with self.assertRaises(ValueError):
                mod.health_verdict(OK, t)


if __name__ == "__main__":
    unittest.main()
