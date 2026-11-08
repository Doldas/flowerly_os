#!/usr/bin/env python3
"""Real SQLite fault contracts for proposed Layer 1.5 adapters (#94).

No mock success substitutes for actual kernel, PTY or delegated Arch evidence.
"""

import sqlite3
import tempfile
import unittest
from pathlib import Path

MIGRATIONS = (
    Path(__file__).resolve().parent.parent / "src/shared/flowerly-core/migrations"
)


def migrated(path):
    conn = sqlite3.connect(path, timeout=0)
    conn.execute("PRAGMA foreign_keys=ON")
    for script in sorted(MIGRATIONS.glob("[0-9][0-9][0-9][0-9]_*.sql")):
        conn.executescript(script.read_text(encoding="utf-8"))
    return conn


def insert_flow(conn, id_):
    conn.execute(
        "INSERT INTO flows(id,name,state,created_by,created_at,owner_class,owner_uid) "
        "VALUES(?, 'contract', 'ACTIVE', 'uid:1000', 1, 'user', 1000)",
        (id_,),
    )


class SqliteDriverContracts(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = str(Path(self.temp.name) / "state.db")
        self.conn = migrated(self.path)
        self.addCleanup(self.conn.close)

    def test_provenance_insert_failure_rolls_back_object(self):
        self.conn.executescript(
            "CREATE TRIGGER reject_provenance BEFORE INSERT ON provenance_edges "
            "BEGIN SELECT RAISE(ABORT, 'injected provenance failure'); END;"
        )
        with self.assertRaisesRegex(sqlite3.IntegrityError, "injected provenance"):
            with self.conn:
                insert_flow(self.conn, "failure-case")
                self.conn.execute(
                    "INSERT INTO provenance_edges(from_ref,relation,to_ref,created_at) "
                    "VALUES('flow:failure-case','created_by','actor:1000',1)"
                )
        self.assertIsNone(
            self.conn.execute("SELECT id FROM flows WHERE id='failure-case'").fetchone()
        )

    def test_foreign_key_rejection_is_not_ignored(self):
        with self.assertRaises(sqlite3.IntegrityError):
            with self.conn:
                self.conn.execute(
                    "INSERT INTO sessions(id,flow_id,kind,state,created_by,created_at,"
                    "owner_class,owner_uid) VALUES("
                    "'bad-ref','absent-flow','shell','PENDING','uid:1000',1,'user',1000)"
                )
        self.assertIsNone(
            self.conn.execute("SELECT id FROM sessions WHERE id='bad-ref'").fetchone()
        )

    def test_operation_receipt_rolls_back_with_failed_mutation(self):
        with self.assertRaises(sqlite3.IntegrityError):
            with self.conn:
                self.conn.execute(
                    "INSERT INTO ipc_operations(actor,command,operation_id,request_payload,"
                    "response_payload,created_at) VALUES('uid:1000','flow.create','k1','{}','{}',1)"
                )
                # Deliberately fail after writing the receipt in same transaction.
                self.conn.execute(
                    "INSERT INTO flows(id,name,state,created_by,created_at,owner_class,owner_uid) "
                    "VALUES('x','x','ACTIVE','uid:1000',1,'wrong',1000)"
                )
        self.assertEqual(
            self.conn.execute(
                "SELECT count(*) FROM ipc_operations WHERE operation_id='k1'"
            ).fetchone()[0],
            0,
        )

    def test_second_writer_cannot_commit_during_immediate_transaction(self):
        other = sqlite3.connect(self.path, timeout=0)
        self.addCleanup(other.close)
        self.conn.execute("BEGIN IMMEDIATE")
        with self.assertRaisesRegex(sqlite3.OperationalError, "locked"):
            other.execute("BEGIN IMMEDIATE")
        self.conn.rollback()
        other.execute("BEGIN IMMEDIATE")
        other.rollback()


if __name__ == "__main__":
    unittest.main()
