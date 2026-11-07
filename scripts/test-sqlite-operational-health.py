#!/usr/bin/env python3
import importlib.util
import sqlite3
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "health", Path(__file__).with_name("sqlite-operational-health.py")
)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)


class OperationalHealthTests(unittest.TestCase):
    def test_real_database_without_mutation(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "state.db"
            db = sqlite3.connect(path)
            db.execute("CREATE TABLE thing(id TEXT PRIMARY KEY)")
            db.execute("INSERT INTO thing VALUES ('private data')")
            db.commit()
            db.close()
            original = path.read_bytes()
            report = mod.inspect(path)
            self.assertEqual(report["status"], "pass")
            self.assertNotIn("private data", str(report))
            self.assertEqual(path.read_bytes(), original)

    def test_rejects_link_and_missing_file(self):
        with tempfile.TemporaryDirectory() as folder:
            file = Path(folder) / "state.db"
            sqlite3.connect(file).close()
            link = Path(folder) / "link"
            link.symlink_to(file)
            with self.assertRaises(ValueError):
                mod.inspect(link)
            with self.assertRaises(ValueError):
                mod.inspect(Path(folder) / "missing")


if __name__ == "__main__":
    unittest.main()
