#!/usr/bin/env python3
"""Verify snapshot preflight never changes legacy authority (#84)."""
import importlib.util
import sqlite3
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "audit_legacy", Path(__file__).with_name("audit-legacy-sqlite-backup.py")
)
audit_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(audit_module)


class LegacyPreflightTests(unittest.TestCase):
    def test_real_backup_is_read_only(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "state.db"
            connection = sqlite3.connect(path)
            for table in sorted(audit_module.REQUIRED):
                connection.execute(f"CREATE TABLE {table}(id TEXT PRIMARY KEY)")
            connection.execute("INSERT INTO flows VALUES ('valuable')")
            connection.commit()
            before = path.read_bytes()
            report = audit_module.audit(path)
            self.assertTrue(report["integrity_ok"])
            self.assertEqual(report["missing_core_tables"], [])
            self.assertEqual(path.read_bytes(), before)
            self.assertEqual(connection.execute("SELECT id FROM flows").fetchone(), ("valuable",))
            connection.close()

    def test_refuses_symlink_and_incomplete_schema(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "db"
            sqlite3.connect(path).close()
            link = Path(directory) / "link"
            link.symlink_to(path)
            with self.assertRaises(ValueError):
                audit_module.audit(link)
            self.assertTrue(audit_module.audit(path)["missing_core_tables"])


if __name__ == "__main__":
    unittest.main()
