#!/usr/bin/env python3
"""Regression tests for the workflow action reference policy."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).with_name('test-workflow-action-pins.py')
spec = importlib.util.spec_from_file_location('workflow_action_pins', SCRIPT)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ActionPinTests(unittest.TestCase):
    def check_reference(self, reference, expected):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            workflows = root / '.github' / 'workflows'
            workflows.mkdir(parents=True)
            (workflows / 'ci.yml').write_text(
                'jobs:\n  test:\n    steps:\n      - uses: ' + reference + '\n',
                encoding='utf-8',
            )
            with patch.object(module, 'ROOT', root), patch.object(module, 'WORKFLOWS', workflows):
                self.assertEqual(module.main(), expected, reference)

    def test_valid_immutable_reference(self):
        self.check_reference('actions/checkout@' + 'a' * 40, 0)

    def test_valid_local_action(self):
        self.check_reference('./.github/actions/check', 0)

    def test_reject_mutable_tag(self):
        self.check_reference('actions/checkout@v4', 1)

    def test_reject_short_sha(self):
        self.check_reference('actions/checkout@' + 'a' * 39, 1)

    def test_reject_non_hex_sha(self):
        self.check_reference('actions/checkout@' + 'g' * 40, 1)

    def test_reject_branch(self):
        self.check_reference('dtolnay/rust-toolchain@stable', 1)

    def test_reject_uppercase_sha(self):
        self.check_reference('actions/checkout@' + 'A' * 40, 1)


if __name__ == '__main__':
    unittest.main()
