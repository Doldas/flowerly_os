#!/usr/bin/env python3
import importlib.util
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "board", Path(__file__).with_name("project-board-reconcile.py")
)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class BoardReconcileTests(unittest.TestCase):
    def test_consistent_snapshot_and_drift(self):
        issues = [{"number": 8, "state": "closed"}, {"number": 31, "state": "open"}]
        board = [{"issue_number": 8, "issue_state": "closed"},
                 {"issue_number": 31, "issue_state": "open"}]
        self.assertEqual(module.reconcile(issues, board)["status"], "consistent_snapshot_only")
        board[1]["issue_state"] = "closed"
        self.assertEqual(module.reconcile(issues, board)["state_mismatches"], [31])
        board.pop()
        self.assertEqual(module.reconcile(issues, board)["missing_on_board"], [31])

    def test_duplicate_and_boolean_identifiers_rejected(self):
        with self.assertRaises(ValueError):
            module.reconcile([{"number": 8}, {"number": 8}], [])
        with self.assertRaises(ValueError):
            module.reconcile([{"number": True}], [])
        with self.assertRaises(ValueError):
            module.reconcile([], [{"issue_number": 2}, {"issue_number": 2}])


if __name__ == "__main__":
    unittest.main()
