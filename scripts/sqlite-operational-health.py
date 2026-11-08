#!/usr/bin/env python3
"""Read-only operator health evidence for existing local SQLite (#89).

Contains NO table rows or user content; not three-mode end-to-end acceptance.
"""

import argparse
import json
import sqlite3
from pathlib import Path


def inspect(path):
    file = Path(path)
    if file.is_symlink() or not file.is_file():
        raise ValueError("database must be an existing regular non-symlink file")
    db = sqlite3.connect(file.as_uri() + "?mode=ro", uri=True, timeout=1)
    try:
        integrity = db.execute("PRAGMA quick_check").fetchall()
        violations = db.execute("PRAGMA foreign_key_check").fetchall()
        version = db.execute("PRAGMA user_version").fetchone()[0]
        journal = db.execute("PRAGMA journal_mode").fetchone()[0]
        return {
            "schema_user_version": version,
            "journal_mode": journal,
            "quick_check_ok": integrity == [("ok",)],
            "foreign_key_violations": len(violations),
            "db_size_bytes": file.stat().st_size,
            "report_kind": "read_only_local_sqlite_health",
            "status": "pass" if integrity == [("ok",)] and not violations else "fail",
        }
    finally:
        db.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("database", help="existing local state.db")
    args = parser.parse_args()
    try:
        report = inspect(args.database)
    except (OSError, sqlite3.Error, ValueError) as error:
        parser.exit(
            1, f"SQLite health inspection unavailable: {type(error).__name__}\n"
        )
    print(json.dumps(report, sort_keys=True))
    if report["status"] != "pass":
        parser.exit(1, "SQLite health inspection failed\n")


if __name__ == "__main__":
    main()
