#!/usr/bin/env python3
"""Offline regression tests of fail-closed security inventory review."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from contextlib import redirect_stdout, redirect_stderr
from io import StringIO

spec = importlib.util.spec_from_file_location(
    "security_alert_triage", Path(__file__).with_name("security-alert-triage.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def inventory(code=(), dep=(), secret=(), errors=()):
    data = {"code_scanning": list(code), "dependabot": list(dep), "secret_scanning": list(secret)}
    return {"alerts": data, "counts": {k: len(v) for k, v in data.items()}, "errors": list(errors)}


class TriageTests(unittest.TestCase):
    def test_empty_snapshot_never_claims_release_ready(self):
        report = module.triage(inventory())
        self.assertEqual(report["decision"], "requires_release_evidence")
        self.assertEqual(report["total_open"], 0)

    def test_code_scanning_high_blocks(self):
        report = module.triage(inventory(code=[{"number": 1, "severity": "high"}]))
        self.assertEqual(report["decision"], "blocked")
        self.assertEqual(report["families"]["code_scanning"]["priority"]["high"], 1)

    def test_secret_scan_never_leaks_details(self):
        row = {"number": 2, "secret": "TOP_SECRET_SENTINEL",
               "secret_type": "private_key", "validity": "active"}
        report = module.triage(inventory(secret=[row]))
        self.assertEqual(report["families"]["secret_scanning"]["priority"]["critical"], 1)
        self.assertNotIn("TOP_SECRET_SENTINEL", json.dumps(report))
        self.assertNotIn("private_key", json.dumps(report))

    def test_unknown_severity_blocks(self):
        report = module.triage(inventory(dep=[{"number": 7, "severity": "unexpected"}]))
        self.assertEqual(report["families"]["dependabot"]["priority"]["unknown"], 1)
        self.assertEqual(report["decision"], "blocked")

    def test_api_failure_blocks_without_echo(self):
        report = module.triage(inventory(errors=["TOKEN_LEAK_SENTINEL"]))
        self.assertEqual(report["decision"], "blocked")
        self.assertNotIn("TOKEN_LEAK_SENTINEL", json.dumps(report))

    def test_missing_family_rejected(self):
        data = inventory()
        del data["alerts"]["dependabot"]
        with self.assertRaises(ValueError):
            module.triage(data)

    def test_count_mismatch_rejected(self):
        data = inventory()
        data["counts"]["code_scanning"] = 2
        with self.assertRaises(ValueError):
            module.triage(data)

    def test_duplicate_ids_rejected(self):
        with self.assertRaises(ValueError):
            module.triage(inventory(code=[{"number": 1}, {"number": 1}]))

    def test_invalid_boolean_id_rejected(self):
        with self.assertRaises(ValueError):
            module.triage(inventory(code=[{"number": True}]))

    def test_cli_does_not_echo_malformed_sensitive_input(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "report.json"
            path.write_text("INVALID_SECRET_LEAK_SENTINEL", encoding="utf-8")
            stdout, stderr = StringIO(), StringIO()
            with redirect_stdout(stdout), redirect_stderr(stderr):
                code = module.main([str(path)])
            self.assertEqual(code, 2)
            self.assertNotIn("INVALID_SECRET_LEAK_SENTINEL", stdout.getvalue() + stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
