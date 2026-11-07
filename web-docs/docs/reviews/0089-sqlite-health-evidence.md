# #89 — local SQLite health evidence

Status 2026-11-07: **read-only local SQLite inspection increment**. `python3 scripts/sqlite-operational-health.py /path/to/state.db` reads an existing regular non-symlink file using SQLite URI read-only mode. It prints only journal mode, schema user_version, DB file size, quick-check and foreign-key violation counts; never prints user records. Missing/symlinked sources fail closed. Tests verify no mutation and no sensitive row content in the report.

This does not replace full integrity_check, WAL checkpoint/backup verification, fault-injection, retention/quotas, SLO and observability in all three modes. CoreOnly/Cluster backend probes, restart/ENOSPC and exact-SHA Arch/KVM acceptance remain pending. Do not interpret mock green as release acceptance.
