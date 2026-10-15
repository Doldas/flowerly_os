# Architecture implementation documentation pack

This is the pre-code review packet for Flowerly OS storage dual-domain and Layer 1.5 isolation work. **These are proposed specifications**, not implemented code, certified security or accepted ADRs.

Read in order:

1. [Existing #91/#82 source inventory](../reviews/0091-0082-boundary-storage-inventory.md) — crate, Linux, schema ownership.
2. [Existing #82 exact atomicity review](../reviews/0082-storage-atomicity-contract.md) — present SQLite transaction boundaries.
3. [Storage contracts and mode policy](storage-contracts-and-mode-policy.md) — #83/#85/#86/#87/#88.
4. [Migration and recovery](storage-migration-and-recovery.md) — #82/#84/#89.
5. [Layer 1.5 boundary and exceptions](layer-15-boundaries-and-exceptions.md) — #91–#95.
6. [Acceptance and evidence matrix](acceptance-evidence-matrix.md) — exact-commit checks and blockers.
7. [Implementation sequence and decisions](implementation-sequence-and-review-decisions.md) — bounded code PR sequence.

Source-of-truth ADRs: [Accepted ADR-0011](../adr/0011-shared-local-state-database.md), [Proposed ADR-0017](../adr/0017-unified-mode-driven-storage.md), [Proposed ADR-0018](../adr/0018-strict-boundary-isolation-layer.md). Existing security architecture also includes ADR-0014 and proposed ADR-0016.

Review/merge of these documents **does not** automatically accept a proposed ADR or permit production database split or release readiness. No changes to #55, release gates, root PTY refusal, or kernel authority are in this packet.
