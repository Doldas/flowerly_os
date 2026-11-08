#!/usr/bin/env python3
import importlib.util
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "memory",
    Path(__file__).resolve().parent.parent / "tools/offline-memory-prototype.py",
)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class OfflineMemoryTests(unittest.TestCase):
    def test_isolation_persistence_and_deterministic_ties(self):
        with tempfile.TemporaryDirectory() as folder:
            file = Path(folder) / "agentic_memory.db"
            db = module.OfflineMemory(file)
            db.put(1000, "b", "private", "v1", 1, [1.0, 0.0], 1)
            db.put(1000, "a", "private", "v1", 1, [1.0, 0.0], 1)
            db.put(2000, "other", "secret", "v1", 1, [1.0, 0.0], 1)
            self.assertEqual(
                [id for _, id in db.search(1000, "v1", 1, [1.0, 0.0])], ["a", "b"]
            )
            self.assertEqual(db.delete(1000, "other"), 0)
            db.close()
            db = module.OfflineMemory(file)
            self.assertEqual(len(db.search(1000, "v1", 1, [1.0, 0.0])), 2)
            self.assertEqual(db.delete(1000, "a"), 1)
            db.close()

    def test_invalid_vectors_and_core_db_guard(self):
        with tempfile.TemporaryDirectory() as folder:
            with self.assertRaises(ValueError):
                module.OfflineMemory(Path(folder) / "state.db")
            db = module.OfflineMemory(Path(folder) / "agentic_memory.db")
            for vector in ([float("nan")], [float("inf")], [0.0], [1, 2]):
                with self.assertRaises((ValueError, module.InvalidEmbedding)):
                    db.put(1000, "bad", "data", "v1", 1, vector, 1) if len(
                        vector
                    ) == 1 else db.search(1000, "v1", 1, [0.0, 0.0])
            with self.assertRaises(ValueError):
                db.put(-1, "bad", "data", "v1", 1, [1.0], 1)
            db.close()


if __name__ == "__main__":
    unittest.main()
