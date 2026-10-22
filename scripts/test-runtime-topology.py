#!/usr/bin/env python3
"""Regression for #105: exactly two OS daemons; external providers are plugins."""
from pathlib import Path
import tomllib
import unittest

ROOT = Path(__file__).resolve().parent.parent
CORE_DAEMONS = {"flowerlyd", "evolutiond"}
LEGACY_DAEMONS = {
    "agenticd", "capabilityd", "flowd", "generationd", "goald",
    "provenanced", "sessiond",
}


class RuntimeTopology(unittest.TestCase):
    def test_workspace_runtime_boundary(self):
        workspace = tomllib.loads((ROOT / "Cargo.toml").read_text())
        members = workspace["workspace"]["members"]
        runtime = {Path(m).name for m in members if m.startswith("src/") and
                   "/shared/" not in m}
        self.assertEqual(runtime & CORE_DAEMONS, CORE_DAEMONS)
        self.assertFalse(runtime & LEGACY_DAEMONS, runtime)
        # providerd is an optional inference provider process, not a core daemon.
        self.assertEqual(runtime - CORE_DAEMONS, {"providerd"})
        self.assertIn("src/shared/flowerly-core", members)
        for name in CORE_DAEMONS:
            self.assertTrue((ROOT / "src" / name / "src/main.rs").is_file())

    def test_legacy_sockets_are_not_listened_to(self):
        source = (ROOT / "src/flowerlyd/src/main.rs").read_text()
        self.assertIn('config.name = "flowerlyd".into()', source)
        self.assertNotIn("run_service(config", source)
        self.assertIn("run_service_with_status(config", source)
        for name in LEGACY_DAEMONS:
            self.assertNotIn('run_service_with_status("' + name + '"', source)

    def test_unprovisioned_kvm_is_not_claimed_as_ready(self):
        source = (ROOT / "src/evolutiond/src/main.rs").read_text()
        self.assertIn('"execution_enabled": false', source)
        self.assertIn('"isolation_verified": false', source)
        self.assertIn("ErrorCode::Unavailable", source)
        provider = (ROOT / "src/providerd/src/main.rs").read_text()
        self.assertIn('"synthetic": true', provider)
        self.assertIn('"promise": "reference-contract-only"', provider)


if __name__ == "__main__":
    unittest.main()
