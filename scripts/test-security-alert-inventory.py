#!/usr/bin/env python3
"""Offline security alert inventory sanitization and failure tests."""
import importlib.util
import json
from pathlib import Path
import unittest
from unittest.mock import patch
import subprocess

spec = importlib.util.spec_from_file_location(
    "security_alert_inventory", Path(__file__).with_name("security-alert-inventory.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class SecurityAlertInventoryTests(unittest.TestCase):
    def test_secret_payload_not_exported(self):
        alert = {"number": 9, "html_url": "https://github.com/org/repo/security/secret-scanning/9",
                 "secret": "NEVER_EXPORT_THIS_VALUE", "secret_type": "private_key",
                 "validity": "active", "created_at": "2026-10-23T00:00:00Z"}
        result = module.sanitize("secret_scanning", alert)
        self.assertEqual(result["number"], 9)
        self.assertNotIn("NEVER_EXPORT_THIS_VALUE", json.dumps(result))
        self.assertNotIn("private_key", json.dumps(result))
        self.assertNotIn("active", json.dumps(result))

    def test_code_scanning_fields(self):
        result = module.sanitize("code_scanning", {
            "number": 1, "rule": {"id": "R1", "security_severity_level": "high"},
            "most_recent_instance": {"location": {"path": "src/main.rs", "start_line": 4}},
        })
        self.assertEqual(result["severity"], "high")
        self.assertEqual(result["path"], "src/main.rs")
        self.assertEqual(result["line"], 4)

    def test_dependabot_fields(self):
        result = module.sanitize("dependabot", {
            "security_advisory": {"ghsa_id": "GHSA-test", "severity": "critical"},
            "dependency": {"package": {"name": "example", "ecosystem": "cargo"}},
        })
        self.assertEqual(result["ghsa_id"], "GHSA-test")
        self.assertEqual(result["package"], "example")

    def test_pagination_flattens_pages(self):
        completed = subprocess.CompletedProcess([], 0, stdout='[[{"number":1}],[{"number":2}]]')
        with patch.object(module.subprocess, "run", return_value=completed) as runner:
            self.assertEqual(len(module.query("org/repo", "code-scanning/alerts")), 2)
            command = runner.call_args.args[0]
            self.assertIn("--paginate", command)
            self.assertIn("--slurp", command)

    def test_api_failure_does_not_leak_stderr(self):
        completed = subprocess.CompletedProcess([], 1, stdout="", stderr="SENSITIVE_TOKEN")
        with patch.object(module.subprocess, "run", return_value=completed):
            with self.assertRaises(RuntimeError) as context:
                module.query("org/repo", "secret-scanning/alerts")
        self.assertNotIn("SENSITIVE_TOKEN", str(context.exception))


if __name__ == "__main__":
    unittest.main()
