# Layer 1.5 and storage transaction inventory — #91 / #82

Status: **Proposed review inventory**, not implementation, testing or ADR acceptance.
Baseline: `release/2` at `5e55dfa75096004e1af46546415a4b779434114c` (merged #90).
Scope: repository source and migration inspection. No runtime changes, no change to release gates or #55.

## Observed dependencies and boundary ownership (#91)

| Current owner | Observed access | Target boundary / decision |
| --- | --- | --- |
| `crates/agentic-storage/src/lib.rs` | `rusqlite::{Connection, Row, TransactionBehavior, ...}`; schema migrations, SQL transactions, busy classification | Keep concrete SQLite in the storage implementation; introduce narrowly typed repository interfaces at an appropriate boundary in #83. Do not publish arbitrary SQL or `rusqlite::Connection` to services. |
| `crates/agentic-core` | Service config, lifecycle and authenticated peer-to-actor mapping; `rusqlite` only under dev-dependencies for tests | Keep peer identity derived from kernel-attested IPC credentials; remove test-only coupling only if contract coverage remains. |
| `crates/agentic-ipc/src/lib.rs` | Tokio Unix sockets, `peer_cred()`, socket filesystem identity, mode `0600`, bounded IO/deadlines and blocking handler execution | **Reviewed exception candidate:** the trusted IPC transport is the authorization/admission boundary. Do not relocate credentials into an arbitrary mockable domain facade or accept client-supplied UID. |
| `services/sessiond/src/main.rs` | `/proc/self/cgroup`, `/sys/fs/cgroup`, delegated root validation, management `cgroup.procs`, `cgroup.subtree_control`, restart `cgroup.kill` | **High-risk extraction candidate, not automatic relocation:** preserve ordering, delegation checks, owned cgroup identity and fail-closed restart. #92/#95 and #72 require real-kernel verification before extraction. |
| `services/sessiond/src/pty.rs` | Trusted supervisor, child process lifecycle, Unix guardian socket, PTY ownership, cgroup context, token/capability command handling, uncommitted guardian reap | **Temporary narrow exception:** keep supervisor and guardian authority together until #50/#72–#74 adversarial acceptance. No generic `kill(pid)` or caller-provided cgroup path facade. |
| `scripts/pty-guardian.py` | Linux-specific PTY and cgroup implementation (outside Rust crates) | Existing security-sensitive runtime driver. Inventory and review together with `sessiond`; retaining an explicit trusted component is preferable to moving unchecked behavior. |
| `services/flowd`, `goald`, `agenticd`, `generationd`, `provenanced` | Cargo dependencies on `agentic-storage`, but no direct `rusqlite` dependency in inspected manifests | Service-to-`Store` coupling is real; avoid treating a manifest-only dependency as proof of no direct OS usage. Preserve existing domain mutations and error classifications during #83. |
| `crates/agentic-types` | Shared domain identifiers/models | Keep driver-agnostic and authority-neutral; don't put filesystem paths, raw SQL or privileged capabilities into public domain objects. |

Inspected: root `Cargo.toml`; `agentic-storage`, `agentic-core`, `agentic-ipc`, `sessiond`, `flowd`, `goald`, `agenticd`, `generationd`, `provenanced` manifests; selected Rust source. A complete syntax-level import inventory over all files is still required before #93 enforces a denylist. Do not label this an exhaustive scan.

### Authority that may not regress

1. `agentic-ipc` admission uses kernel `peer_cred` plus owning socket UID, not request-supplied identities; socket ownership is inode/device-aware and stale sockets require explicit recovery.
2. `agentic-core` constructs actor identity from authenticated peer UID, not the JSON request.
3. `sessiond` requires real delegated cgroup v2 and disables PTY spawning on failed provisioning; default production mode is `required`, not permissive development mode.
4. PTY control/writer capabilities, one-writer/observer semantics, bounded replay, startup uncertainty and guardian lifecycle remain owned by trusted supervision.
5. Guardian/descendant containment must remain kernel-authoritative across `setsid`, same-UID sibling migration, guardian SIGKILL, `sessiond` SIGKILL and restart. #72/#73/#74 are open. No positive delegated Arch result is inferred from source or mocks.
6. No root-owned interactive PTY, unverified PID-based authority, path-derived mount authorization, silent no-op success, or fabricated telemetry.

Proposed interface placement: storage-facing **domain traits** live where `agentic-types` and error contracts are available without `rusqlite`; implementations in `agentic-storage`. IPC peer authentication and the existing PTY trusted supervisor remain explicit security components. A future host facade should accept an unforgeable, owner-bound allocation capability created by a trusted authority, not `pid: i32`, `PathBuf` or an unrestricted cgroup name. Confirm privilege and lifecycle model before moving code.

## Existing physical schema and transaction ownership (#82)

Evidence: `crates/agentic-storage/migrations/0001_init.sql` through `0005_idempotent_operations.sql`, `crates/agentic-storage/src/lib.rs`, and accepted ADR-0011.

| Table(s) | Existing reference / invariant | Preliminary ownership; migration decision |
| --- | --- | --- |
| `flows`, `projects`, `sessions` | `projects.flow_id` and `sessions.flow_id` reference `flows.id`; `flows.parent_flow` self-reference | Keep in local system core transaction domain. |
| `goals`, `agents`, `executions` | `agents.goal_id`, `executions.goal_id` reference goals; `executions.agent_id` references agents | **Do not move agents/executions merely because they are agent-related.** Currently linked to goals, generations and provenance; require explicit contract before deciding. |
| `generations`, `experiments` | `generations.reason_goal` and `experiments.goal_id` reference goals; generation parent references generations | Keep boot/promotion/provenance authority in system core. Review whether experiments can ever split without cross-file FK loss. |
| `observations`, `findings`, `decisions`, `artifacts` | IDs and provenance references; causal graph is currently cross-domain | Keep in shared core unless a separate durability/provenance protocol is designed and approved. |
| `provenance_edges` | Typed `from_ref`/`to_ref`, unique triple and lookup indexes; not SQL foreign keys to every referenced table | Core authority; must retain creation+provenance atomicity and application-level object-reference validation. |
| `user_facts`, `user_observations`, `user_inferences` | Present in original shared schema | Candidate future memory data, **not approved for migration**; audit consumers, references, privacy, lifecycle and recovery before choosing. |
| `ipc_operations` | Composite key (`actor`, `command`, `operation_id`); receipt/response committed together with mutation | Keep colocated with command's atomic core state; moving the registry breaks idempotency guarantees. |
| `schema_migrations` | Versioned, serialized `IMMEDIATE` migration record | Migration metadata belongs to each owned physical schema after an explicitly tested cutover; no blind copy. |

Additional DB-level constraints: partial unique index for a single `FOREGROUND` flow (0003); unique/protected General Flow triggers (0004); actor prefix normalization (0002). Preserve these during any refactor.

### Transaction groups that must remain atomic

- Flow, Goal, Session and Execution creation plus required provenance edges (inspect each API's exact write path before trait extraction).
- Actor-scoped `ipc_operations` receipt plus the corresponding domain creation and replay response.
- General Flow creation/recovery plus its provenance and foreground state; a single foreground Flow invariant across connections.
- Generation activation: supersede former Stable and promote candidate in one local transaction.
- Migration schema update plus `schema_migrations` version insert, serialized with `BEGIN IMMEDIATE`.
- Referentially related graph operations while SQLite foreign keys enforce local constraints. Splitting any of these across physical DBs is **blocked** until explicitly redesigned and fault-injection verified.

`AGENTIC_DATABASE_PATH` stays a recovery/test override during the entire transition. Never repoint an existing installation silently or delete `state.db` on first boot.

### Interface proposals (no code yet)

- `SystemStateRepository`: typed `create_flow_with_provenance`, `create_goal_with_provenance`, `create_session_with_provenance`, `activate_generation`, `reconcile_session` with explicit actor/capability context.
- `CoreUnitOfWork` or command-level repository: a **single owned SQLite write transaction** that encloses provenance, idempotency receipt and core mutation; avoid separate cross-repository `commit()` calls.
- `MemoryRepository` and `EmbeddingIndex`: separate opt-in namespace, actor ACL and typed values; no core-boot dependency and no SQL passthrough.
- `ReplicationController`: memory-only; opt-in authenticated peers; no core writes or implicit network startup.
- `InstallationMode` = `Standalone | Cluster | CoreOnly`; `MemoryBackend` = `Disabled | LocalSqlite | VerifiedReplicated`. These are **orthogonal**: CoreOnly requires Disabled; Standalone requires local-only memory if enabled; Cluster may select a verified replicated memory backend. Neither type alone authorizes network access.

Types are proposals, not trait implementations. Error categories should preserve `Busy/Locked`, unavailable delegation/backend, unauthorized, invalid input, uncertain commit and corruption. No `Ok(())` for unperformed privileged effects.

## Proof obligations and gated order

Portable/mock: typed query validation, authorization denial, idempotency replay, transaction fault injection, unavailable-memory boot decisions, malformed mode, mocked timeout/permission failure, and prohibited driver import checks.

Real SQLite: FK/triggers, single foreground flow, `BEGIN IMMEDIATE` concurrency, WAL/backups, atomic provenance and idempotency, disk-full, simulated crash during migration and restart recovery.

Real Linux/Arch/KVM: socket peer UID, guardian capability and multi-client stream races, delegated cgroup escape attempts, `setsid` grandchildren, same-UID sibling migration, PID reuse, guardian/`sessiond` SIGKILL, restart, root rejection and no new network listeners. Link all evidence to **exact tested SHA and job URL**. Guest package upgrade failure before security matrix means **not tested**, not pass.

Ordering: owner review of ADR-0017/0018; finish #91/#82 review; #83 focused interfaces without physical split; #92/#95 only when #50/#72 safety constraints are preserved; #93/#94 verified contracts; #84 physical split only after proven recovery and ACID design. #55 explicitly out of scope.

## Verification and decision log

- **Proposed:** this mapping and interface strategy.
- **Implemented:** pre-existing shared SQLite `Store`, migrations and Linux/PTY runtime source only; this PR introduces no runtime features.
- **Tested:** no new tests executed by this document-only analysis; CI status belongs exclusively to its reported commit and workflow.
- **Accepted:** ADR-0011 remains Accepted. ADR-0017/0018 remain Proposed even though PR #90 was merged.
- **Blockers:** release guest disk-full prevents delegated positive test (#79); unresolved #50/#72–#74; no approved cross-database atomicity, rollback or storage-migration plan; owner architecture decision pending.
