# Layer 1.5 implementation inventory (issue #91)

Status: **Proposed for maintainer/security review**. Snapshot audited against
`release/2` commit `218edd56c247741faf8b7dce1180a0dddf8c29ba`.
This is not an implemented dependency firewall or accepted ADR.

## Observed boundaries and allowed exceptions

| Location | Concrete dependency | Present owner / disposition |
| --- | --- | --- |
| `src/shared/flowerly-core/src/storage/mod.rs` | `rusqlite::{Connection, TransactionBehavior, Row, params}`, SQLite errors and migrations | **Existing debt**: one shared, cross-domain ACID store under accepted ADR-0011. Move driver details behind domain-typed repository interfaces only after #82/#83 migration proof; never expose arbitrary SQL to higher layers |
| `src/shared/flowerly-core/src/ipc.rs` | Unix socket transport, Unix metadata and kernel-attested peer credentials | **Reviewed exception**: authenticated IPC framing and peer identity remain in transport foundation; not a generalized storage/host facade |
| `src/flowerlyd/src/session/pty.rs` | Unix streams, shell process launch, guardian and cgroup parameters | **Security-critical exception**: preserve trusted PTY owner, parent PID, writer-token checks and kernel containment. No interface-only refactor without #95 threat tests |
| `src/flowerlyd/src/session/runtime.rs` | Delegated cgroup preparation, stale session recovery and cgroup termination | **Existing debt / required authority**: domain flow invokes Linux supervisor helpers. Isolate vetted operations behind strongly typed, ownership-checked contracts during #92/#95; never accept arbitrary caller-supplied PID or cgroup path |
| `src/evolutiond/src/kvm/mod.rs` | Separate KVM boundary placeholder | **Not provisioned**: any privileged VM/host adapter must live in the separate, authorized `evolutiond` process after broker and guest security review; placeholder is not positive KVM evidence |
| `src/flowerlyd/src/{flow,goal,generation,provenance,agentic,compute}/` | Domain command handlers and durable store calls | **Policy target**: consume typed repository operations. Do not move raw SQLite driver handles into these modules |
| `src/providerd/` | Optional synthetic local inference provider | **Process-level exception** under ADR-0021; not a trusted OS authority or a Layer 1.5 Linux driver |

This is a *targeted source inventory*, not a machine-generated exhaustive
import graph. Re-run dependency discovery when a relevant source file changes.

## Trait ownership and staged migration decision (proposed)

1. Place **domain-typed storage interfaces** with domain types in
   `src/shared/flowerly-core`; place concrete SQLite adapters in a separate
   implementation module/crate only when #83 can verify identical transactions,
   foreign keys, actor binding, error typing, and replay behavior.
2. Place **Linux host facade traits** with explicit authorized operations at
   the supervisor/host boundary, with Linux driver implementations kept beside
   the trusted PTY owner or a future adapter under #92. Domain callers must
   submit validated session identities/handles, *not arbitrary paths, PIDs,
   raw SQL, cgroup names or untrusted capability descriptors*.
3. Keep authenticated IPC, kernel peer credentials, guardian lifecycle,
   process death/cgroup cleanup and restricted Evolution process access as
   documented exceptions pending #95 adversarial verification. Moving imports
   alone does not make privilege enforcement real.
4. Run #93 dependency import gates **after** the inventory is reviewed and
   baselined. Record existing exceptions rather than globally banning Linux
   imports. Run #94 mock/real fault contracts and actual delegated Arch/KVM
   evidence before accepting adapters. No `todo!()` or fabricated success
   values in production host methods.

## Compatibility and falsifiable tests

- ADR-0011 remains the current accepted shared `state.db` owner; proposed
  ADR-0017 does **not** authorize dual-database migration or replication.
- ADR-0014 remains authoritative: Linux/kernel checks cannot be replaced by
  mock facades. ADR-0016 PTY supervision and ADR-0019 two-daemon layout remain
  distinct; proposed Layer 1.5 does not authorize a third privileged daemon.
- Negative acceptance: reject unauthorized UID/PID, stale PID, symlink/cgroup
  migration, unavailable delegated scopes, transaction conflict and corrupted
  SQLite; no fabricated telemetry or zero-effect mount success.
- Positive acceptance: exact actor ownership, bounded IPC, transaction replay,
  PTY descendant cleanup on real kernel, and independent release-branch Arch
  guest evidence on exact commit. CI unit tests alone are insufficient.

**Owner decision required before ADR-0018 can become Accepted.** This document
does not implement #83/#92/#93/#94/#95 or alter release exit criteria.
