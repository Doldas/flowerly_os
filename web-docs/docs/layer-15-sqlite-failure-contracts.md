# Layer 1.5 SQLite driver fault-contract tranche (#94)

Status 2026-11-07: **offline real-SQLite tests proposed**, not an approved
mock/real adapter parity matrix or privileged release acceptance.

`python3 scripts/test-layer-15-sqlite-contracts.py` creates a temporary
file-backed database, applies all current migrations and asserts:
- a failure to insert required provenance rolls back a newly created Flow
  in the same transaction (no partial durable object);
- a missing Flow FK rejects a Session, rather than silently accepting it;
- an overlapping `BEGIN IMMEDIATE` fails with busy/locked and the
  second writer can acquire the lock after rollback.

The test is included in `scripts/preflight-docs.sh`. Each test uses a real
SQLite engine; these are driver-level regression contracts, **not evidence
that every production command uses the same transaction boundary**.

Remaining #94 acceptance: typed and mock adapter parity for storage/telemetry,
deadline and disconnect injection, backpressure and async blocking measurement,
full real process failure contracts, and exact-head privileged Arch/PTY tests.
No mock can replace cgroup/PTY/permission-denied kernel evidence.
