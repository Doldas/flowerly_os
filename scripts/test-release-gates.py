#!/usr/bin/env python3
"""Regression tests for the release-branch policy validator (offline)."""
import copy
import json
from pathlib import Path
import tempfile
import unittest
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import importlib.util

SCRIPT = Path(__file__).with_name("release-gates.py")
spec = importlib.util.spec_from_file_location("release_gates", SCRIPT)
gates = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gates)


class ReleaseGateTests(unittest.TestCase):
    def test_checked_in_policy_and_workflows(self):
        gates.validate()

    def _copy(self, root):
        files = (
            ".github/release-gates.json",
            ".github/workflows/ci.yml",
            ".github/workflows/security.yml",
            ".github/workflows/codeql.yml",
            ".github/workflows/pr-validation.yml",
        )
        for filename in files:
            target = root / filename
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(
                (gates.ROOT / filename).read_text(encoding="utf-8"),
                encoding="utf-8",
            )

    def test_missing_required_security_check_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._copy(root)
            config = root / gates.POLICY_FILE
            data = json.loads(config.read_text())
            data["rules"][1]["parameters"]["required_status_checks"].pop(3)
            config.write_text(json.dumps(data))
            with self.assertRaisesRegex(ValueError, "required check contexts drifted"):
                gates.validate(root)

    def test_security_path_filter_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._copy(root)
            security = root / ".github/workflows/security.yml"
            security.write_text(security.read_text() + "\npaths-ignore: ['docs/**']\n")
            with self.assertRaisesRegex(ValueError, "cannot be skipped"):
                gates.validate(root)

    def test_advisory_soft_fail_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._copy(root)
            security = root / ".github/workflows/security.yml"
            security.write_text(security.read_text() + "\ncontinue-on-error: true\n")
            with self.assertRaisesRegex(ValueError, "soft-fail"):
                gates.validate(root)

    def test_bypass_actor_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self._copy(root)
            config = root / gates.POLICY_FILE
            data = json.loads(config.read_text())
            data["bypass_actors"] = [{"actor_type": "User", "actor_id": 1}]
            config.write_text(json.dumps(data))
            with self.assertRaisesRegex(ValueError, "bypass actors"):
                gates.validate(root)

    def test_remote_missing_check_is_rejected(self):
        policy = json.loads((gates.ROOT / gates.POLICY_FILE).read_text())
        actual = copy.deepcopy(policy)
        actual["rules"][1]["parameters"]["required_status_checks"].pop()
        with self.assertRaisesRegex(ValueError, "remote required check list mismatch"):
            gates.assert_rule_enforcement(policy, actual)


if __name__ == "__main__":
    unittest.main()
