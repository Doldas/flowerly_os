#!/usr/bin/env python3
"""Fail-closed physical SQLite schema inventory for proposed storage split (#82).

Only schema-level FKs are discoverable here. Application-level references and
transaction boundaries require a separate manual review before any split.
"""

import json
import sqlite3
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MIGRATIONS = ROOT / "src/shared/flowerly-core/migrations"
# Not approved for relocation: these are candidate review markers only.
MEMORY_CANDIDATES = {"user_facts", "user_observations", "user_inferences"}
CORE_REQUIRED = {"flows", "goals", "sessions", "ipc_operations", "provenance_edges"}


def inspect(conn):
    tables = sorted(
        name
        for (name,) in conn.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'"
        )
    )
    edges = []
    for table in tables:
        # Names come from sqlite_master, not user input.
        quoted = '"' + table.replace('"', '""') + '"'
        for fk in conn.execute(f"PRAGMA foreign_key_list({quoted})"):
            edges.append((table, fk[2], fk[3], fk[4]))
    return tables, sorted(edges)


def audit(tables, edges):
    problems = []
    missing = CORE_REQUIRED.difference(tables)
    if missing:
        problems.append("missing core tables: " + ", ".join(sorted(missing)))
    # Any direct FK between candidate memory and core requires an explicit
    # atomicity design; never treat the SQL graph as proof of safe migration.
    for source, target, _, _ in edges:
        if (source in MEMORY_CANDIDATES) != (target in MEMORY_CANDIDATES):
            problems.append(f"cross-boundary foreign key: {source} -> {target}")
    return problems


def main():
    migrations = sorted(MIGRATIONS.glob("[0-9][0-9][0-9][0-9]_*.sql"))
    if not migrations:
        raise SystemExit("no schema migrations found")
    conn = sqlite3.connect(":memory:")
    conn.execute("PRAGMA foreign_keys = ON")
    for migration in migrations:
        try:
            conn.executescript(migration.read_text(encoding="utf-8"))
        except sqlite3.Error as error:
            raise SystemExit(f"{migration.name} did not apply: {error}") from error
    tables, edges = inspect(conn)
    problems = audit(tables, edges)
    report = {
        "status": "review_only_not_approved_for_split",
        "migrations": [path.name for path in migrations],
        "tables": tables,
        "foreign_keys": [
            {"from": src, "to": dst, "column": col, "target_column": dstcol}
            for src, dst, col, dstcol in edges
        ],
        "memory_candidates_not_approved": sorted(
            MEMORY_CANDIDATES.intersection(tables)
        ),
        "warnings": [
            "No app-level provenance, IPC receipt or transaction coupling is proven by this report",
            "ADR-0011 remains authoritative; never split physical databases based on this audit",
        ],
        "problems": problems,
    }
    print(json.dumps(report, indent=2, sort_keys=True))
    if problems:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
