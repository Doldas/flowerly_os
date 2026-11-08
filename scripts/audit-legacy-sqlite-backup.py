#!/usr/bin/env python3
"""Read-only legacy SQLite backup preflight; NEVER switches authority (#84)."""

import argparse
import json
import sqlite3
import stat
import tempfile
from pathlib import Path

REQUIRED = {"flows", "goals", "sessions", "ipc_operations", "provenance_edges"}


def audit(path):
    file = Path(path)
    info = file.lstat()
    if not stat.S_ISREG(info.st_mode):
        raise ValueError("state database must be a regular file, not a symlink")
    # mode=ro, immutable is deliberately NOT used: the source may have WAL.
    source = sqlite3.connect(file.as_uri() + "?mode=ro", uri=True, timeout=2)
    try:
        with tempfile.TemporaryDirectory() as tmp:
            target = sqlite3.connect(str(Path(tmp) / "snapshot.db"))
            try:
                source.backup(target, pages=128)
                integrity = target.execute("PRAGMA integrity_check").fetchall()
                foreign_keys = target.execute("PRAGMA foreign_key_check").fetchall()
                tables = {
                    r[0]
                    for r in target.execute(
                        "SELECT name FROM sqlite_master WHERE type='table'"
                    )
                }
                missing = sorted(REQUIRED - tables)
                # No row values or user contents are emitted.
                return {
                    "integrity_ok": integrity == [("ok",)],
                    "foreign_key_violations": len(foreign_keys),
                    "missing_core_tables": missing,
                    "source_bytes": info.st_size,
                    "status": "inspection_only_no_cutover",
                }
            finally:
                target.close()
    finally:
        source.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("state_db", help="existing, offline or quiesced state.db")
    args = parser.parse_args()
    try:
        report = audit(
            Path(args.state_db).resolve(strict=True)
            if not Path(args.state_db).is_symlink()
            else Path(args.state_db)
        )
    except (OSError, sqlite3.Error, ValueError) as exc:
        parser.exit(1, f"migration preflight refused: {type(exc).__name__}\n")
    print(json.dumps(report, sort_keys=True))
    if (
        not report["integrity_ok"]
        or report["foreign_key_violations"]
        or report["missing_core_tables"]
    ):
        parser.exit(1, "migration preflight: failed integrity or schema checks\n")


if __name__ == "__main__":
    main()
