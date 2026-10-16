# #82 — Atomicity and migration contract review (step 2)

Status: **Proposed**; review/decision document only. Baseline `release/2` commit `7d9960ba00371424599f1f6d8205fe8f9826e79a`.
Companion: [#91/#82 inventory](0091-0082-boundary-storage-inventory.md).
ADR-0011 is **Accepted**; ADR-0017 and ADR-0018 are **Proposed**.

## Exact observed single-file write boundaries

Source: `crates/agentic-storage/src/lib.rs` on the baseline commit. These names refer to the present `Store` implementation, not to hypothetical traits.

| Operation | Source range | SQLite boundary | Must preserve |
| --- | --- | --- | --- |
| `migrate` | ~141–181 | `TransactionBehavior::Immediate`, apply schema + record migration version, commit | Concurrent migrators serialize, no successful partially applied version |
| `ensure_general_flow` | ~221–273 | `Immediate` | General bootstrap, created-by provenance, active foreground decision together |
| `insert_once` | ~280–345 | `Immediate` | Actor/command/idempotency-key replay registry and domain write; do not split operation receipt from effect |
| `insert_flow_with_provenance_once` / goal / session | ~349–408 | invoke `insert_once` | Same atomicity and authenticated actor context |
| `insert_flow_with_provenance` | ~413–426 | `Immediate` | Core object and provenance edges commit or roll back together |
| `activate_flow` | ~452–476 | `Immediate` | Old and new foreground state; preserve unique partial index |
| `insert_goal_with_provenance` | ~500–513 | `Immediate` | Goal and provenance |
| `insert_session_with_provenance` | ~552–565 | `Immediate` | Session and provenance, without claiming PTY process spawn participates in SQLite transaction |
| `insert_execution_with_provenance` | ~619–632 | `Immediate` | Execution and provenance |
| `insert_generation_with_provenance` | ~660–673 | `Immediate` | Generation and provenance |
| `activate_generation` | ~710–742 | `Immediate` | Exactly one Stable generation; failed candidate activation must preserve previous Stable |

Important distinction: methods such as `insert_flow`, `insert_goal`, `insert_session`, `insert_execution` and `insert_generation` also exist as non-provenance write APIs. A trait that exposes only these methods would allow bypassing required provenance. Audit existing callers and distinguish permitted internal/bootstrap paths from user-driven, actor-authenticated mutations before #83.

## Physical relocation decision — currently blocked

| Existing tables | Provisional location | Reason / unresolved issue |
| --- | --- | --- |
| flows, projects, sessions | Current shared core (`state.db`); future local system core | Foreign keys and lifecycle |
| goals, agents, executions | Remain together with local core pending review | Cross-domain foreign keys and Generation goal references |
| generations, experiments | Remain together with local core pending review | Goal FKs; boot/activation/provenance |
| observations, findings, decisions, artifacts, provenance_edges | Retain shared core for now | Generic causal edges and retention/integrity require ownership review |
| ipc_operations | Co-located with each affected mutation | Idempotency must be transactional with command side effects |
| user_facts, user_observations, user_inferences | **Candidate only**, no approved move | No verified consumers/privacy migration contract yet |
| schema_migrations | Retain current core migration history | Requires new versioned protocol for any future second schema |

Existing declared cross-table FKs in initial migration include: `projects.flow_id → flows`, `sessions.flow_id → flows`, `agents.goal_id → goals`, `executions.agent_id → agents`, `executions.goal_id → goals`, `generations.reason_goal → goals`, `experiments.goal_id → goals`. Self-links exist for flows/goals/generations. The `provenance_edges` table encodes typed object references, not a blanket SQLite FK to each domain table.

Constraints to preserve: `idx_flows_single_foreground`, protected/unique General Flow, migration-0002 actor normalization, `ipc_operations` composite key.

## #83 implementation seam proposed

Prefer a **command-scoped core repository** with one atomic method per domain command and typed input/output/error; internal implementation holds the existing `rusqlite::Transaction`. Avoid exposing a generic `TransactionDelta`, `RecordSet`, connection handle, arbitrary SQL, `begin/commit` pair spanning independent repositories, or public `fn write(&Connection)`.

Suggested capability split:

- `CoreCommandRepository`: create Flow/Goal/Session/Execution/Generation with provenance and actor-bound idempotency; activate Flow or Generation; reconcile Session durable state.
- `CoreQueryRepository`: typed read-only lookups, graph traversal and health queries.
- `MemoryRepository` / `EmbeddingIndex`: future optional memory-only interfaces, with no dependency from core boot paths.
- `ReplicationController`: future explicit memory replication only; no system-core mutation.

These are interface recommendations, **not implemented APIs**. A synchronous `Store` is currently guarded/used from service handlers; blocking SQL execution must remain isolated from async event loops.

Two different axes are required: `InstallationMode` (Standalone/Cluster/CoreOnly) versus `MemoryBackend` (Disabled/LocalSqlite/VerifiedReplicated). The mode policy must validate their combinations and fail closed; picking Cluster never automatically opens a port or authorizes a peer.

## Reproducible acceptance matrix (future work)

| Invariant | Failure injection | Required outcome | Required environment |
| --- | --- | --- | --- |
| Object + provenance | Reject edge insert after successful object insert inside same transaction | Neither object nor edge visible on other connection after error | Real SQLite |
| Idempotency receipt + object | Fail object insert or receipt insert; retry same authenticated actor+key | No receipt without object, no duplicate committed object | Real SQLite, concurrent writers |
| General/foreground | Two connections try concurrent foreground activation | At most one foreground; General immutable | Real SQLite, two connections |
| Generation promotion | Missing candidate, SQL write failure between demote/promote, process interruption | Prior Stable remains or committed transition has exactly one Stable | Real SQLite, crash/fault injection |
| Migration versions | Two startup migrators, interrupted schema step, incompatible version | Version/schema agree; no unsafe implicit destructive retry | Real SQLite, file-backed + restart |
| Future physical cutover | SIGKILL at backup, copy, verify, cutover, restart; disk full, WAL present | Authoritative old or new version unambiguous; rollback is recoverable; no lost provenance/receipts | Arch/KVM with actual disk/filesystem |
| Optional memory boot | Missing/corrupt memory file, network down, disabled backend | Core boot unaffected; no implicit Cluster fallback | Arch/KVM |
| PTY authority | Same-UID sibling cgroup migration, `setsid` descendant, daemon SIGKILL | Kernel-backed containment and truthful recovery; mocked success insufficient | Real delegated Arch kernel, #72/#74 |

No tests have been executed **by this document**. Passing existing unit tests or documentation checks is not evidence that fault scenarios above have been performed.

## Preconditions before #84

1. Identify actual readers/writers of every candidate memory table, including future compatibility and deletion semantics.
2. Prove a one-database transaction boundary for every current core invariant and specify how future separate domains avoid shared foreign keys.
3. Choose a crash-consistent migration protocol, old-file preservation period, deterministic recoverability and `AGENTIC_DATABASE_PATH` override semantics.
4. Define exact failure-injection scripts, expected ledger/row counts, and required architecture/owner decision.
5. Leave security/release gates, #55 and all PTY trust boundaries unchanged.

**Decision requested:** approve the domain/transaction ownership mapping separately from any physical split. Neither this review nor a green PR check accepts ADR-0017.
