# #84 — legacy SQLite backup preflight

Status 2026-11-07: **read-only operator inspection only**. Run `python3 scripts/audit-legacy-sqlite-backup.py /path/to/state.db` on a quiesced or maintenance-mode installation. It opens the actual source read-only, uses the SQLite backup API to create a disposable snapshot, checks integrity, schema presence, and FK count, and prints counts/status only, never user rows. Symlinked/nonregular input is refused. Regression tests verify no source byte mutation and rejected invalid inputs.

This tool **never creates system_state.db or agentic_memory.db, writes cutover manifests, renames files, or changes authority**. The two-file ACID design, durable crash-safe protocol, WAL and rollback evidence, full migration matrix, owner review and real Arch/KVM acceptance are still prerequisites. ADR-0011 remains authoritative.
