# Shared SQLite to dual-domain migration and recovery protocol

**Status:** Proposed design gate for #82 #84 #89, **not an approved cutover algorithm**. ADR-0011 remains Accepted. There is no permission to rename, delete or detach `state.db`.

## Inventory constraints

The current source of truth is `crates/agentic-storage/migrations/0001_init.sql` through `0005_idempotent_operations.sql` and `Store` in `src/lib.rs`. Mandatory atomics: object and provenance insert; `ipc_operations` actor/command/key receipt with effect; General creation and single foreground Flow; generation promotion/demotion; migration version and schema. Existing cross-table FKs include projects/sessions -> flows; agents/executions/experiments/generations -> goals; executions -> agents. Provenance has typed application-level references, not blanket relational FKs. Do **not** assume tables prefixed `user_` are safe to extract without a consumer, retention and authority audit.

## Pre-cutover investigation checklist

1. Catalogue every table/column/index/trigger/FK, migration version, read and write callsite, actor authorization check, backup/restore contract and sensitive data classification. Record owner and whether a strong transaction spans domains.
2. For each candidate memory table, demonstrate that its move cannot invalidate boot, generation rollback, provenance integrity or idempotency. Require explicit owner signoff and an acceptance test. Unknown => retain in core.
3. Measure actual WAL behavior, backup consistency, filesystem sync guarantees and disk-space requirements under the target Arch filesystem. Never rely on a pair of independent file renames as a universal atomic two-file commit.
4. Define a written recovery state machine, crash points, durable manifest/checksum, read-only old-store preservation, rollback eligibility, version compatibility and upgrade/downgrade policy before writing migration code.

## Candidate offline-only state machine (requires validation before implementation)

`LegacyAuthoritative` -> `QuiescedAndBackedUp` -> `Staged` -> `Validated` -> `CutoverIntentDurable` -> `NewAuthoritative` -> `RetiredAfterRetention`.

* Acquire an exclusive, observable maintenance state; stop all writers including daemon restarts. Abort if quiescence cannot be proven.
* Use SQLite backup/checkpoint protocol rather than copying a live WAL file naively. Record schema versions, exact row counts, FK checks, integrity checks, object/edge linkage, receipt consistency and cryptographic digests of staged content.
* Store a durable recovery manifest describing which database generation is authoritative. Before cutover, require enough disk to hold verified old state, staged state, WAL/journals and reserve; abort cleanly on ENOSPC.
* Advance authoritative state only after validation, fsync and recovery proof on the exact target filesystem. Reject mixed-version boot rather than opening partly migrated files.
* On ambiguous cutover or manifest corruption, **fail closed** to maintenance/recovery; do not choose the newest timestamp and do not destroy old state. A rollback may be forbidden once writes to new state begin; explicitly bound the rollback window and provide a separately tested forward-recovery route.
* Never replicate `system_state.db`. A missing or broken `agentic_memory.db` must not prevent local core boot after a demonstrably safe cutover.

This is **not** a promise that the candidate protocol already guarantees crash consistency. Choose and prove actual manifest atomicity and fsync ordering before #84.

## Fault and recovery matrix

| Inject at | Prove |
| --- | --- |
| Before writer quiescence / racing writers | No conflicting writes; no hidden half-freeze |
| During SQLite backup/WAL checkpoint | Original authority intact |
| Every schema conversion and row-copy step | Referential integrity, receipt/provenance counts, rerun policy |
| After staged data but before durable cutover | Old state always uniquely authoritative |
| During durable cutover / torn writes | Recovery chooses an unambiguous consistent state, otherwise refuses boot mutation |
| Immediately after cutover / after first new write | No unsafe rollback or double-apply |
| ENOSPC, EIO, permission-denied, wrong owner, corrupt manifest | No silent success, no dropped records |
| Restart with `AGENTIC_DATABASE_PATH` override | Explicit legacy recovery semantics, no implicit split |
| Missing/corrupted optional memory | Local core remains bootable; memory disabled |

## Acceptance evidence

Before changing physical files require owner approval of every table classification, a reproducible migration script and its rollback/forward-recovery plan, real SQLite fault-injection logs, WAL/integrity checks and exact-SHA Arch/KVM crash tests. No automatic acceptance from a green docs PR.
