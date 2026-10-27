# IPC mutation outcomes, retries and SQLite contention

Status: **implemented SQLite retries, externally effective PTY safety under review** (issues #62 and #63). The original durable SQLite create-key implementation was merged in PRs #66–#67; PR #156 adds separately scoped PTY guardrails.

## What current IPC responses guarantee

- Each request uses a correlated IPC v1 response ID and has a bounded network/queue/handler response deadline.
- The worker holding an IPC handler slot continues until its blocking SQLite work returns, even if the client has already received a timeout. A timeout does **not** prove a database transaction was rolled back.
- `unavailable` caused by SQLite BUSY/LOCKED means SQLite rejected the operation under lock contention. `unavailable` from an overall IPC deadline **can have an unknown mutation outcome**.
- Deadline `unavailable` messages name the stage and elapsed milliseconds. "while queued for a handler; the request was not executed" means the handler never started. "while the handler was running; the outcome is unknown" and the `flowerlyd` module "the outcome is unknown" mean the mutation may still commit. Clients must branch on the error `code`; the message is a diagnostic, not a contract.
- Each request produces one structured `ipc request` log event with `request_id`, the command name (never its arguments), `peer_uid`, `queue_ms`, `handler_ms`, `total_ms`, `outcome` (`ok`, `error`, `deadline_queued`, `deadline_running`) and `code`. It logs at `warn` for deadline expiry or requests taking 1 s or more, and at `debug` otherwise.
- The envelope correlation ID is not a durable idempotency key. Legacy calls with no explicit `operation_id` remain non-idempotent. Merged PRs #66–#67 introduced opt-in, actor-scoped durable results for SQLite `flow.create`, `goal.create` and metadata-only `session.create`; it does not cover externally visible side effects.
- Read-after-timeout reconciliation is possible in some cases by listing or querying domain records, but names and goals are not guaranteed unique and that alone is **not** a correctness proof of idempotency.

## Safe interim client policy

1. Never interpret a timed-out mutation as "not applied".
2. Do not automatically retry a mutation without an explicit supported `operation_id`; retries with a key must reuse the **same key and exact same command arguments**, within the documented retention window.
3. Report **outcome unknown** distinctly in client UX/logging and recommend explicit reconciliation where possible. For a critical or automated workflow, stop pending an authoritative result.
4. Include correlation ID, actor identity, command name, elapsed time and response classification in diagnostics without logging secrets or full sensitive payloads.
5. No agent may guess that a task is safe to repeat merely because it is conceptually a "retry".

## Opt-in durable create-operation identity (merged PRs #66–#67)

For `flow.create`, `goal.create`, `goal.child`, and `session.create` (metadata-only), specify an optional `operation_id` string with 1–128 ASCII letters, numbers, dot, underscore, colon or hyphen. Null or omitted means legacy non-idempotent behavior; other JSON types are rejected. Keyed request/response bodies are limited to 64 KiB each; oversized requests are rejected with `invalid_argument`. The same operation ID must be reused on retry, not regenerated. A CLI user may pass `agenticctl flow create "Plan" --operation-id my-request-123`, with analogous `goal create` and `session create` options.

Under the Unix-socket service boundary, the trusted actor is derived from kernel-attested peer UID and overrides caller-supplied `created_by`. The SQLite key is `(trusted actor, command, operation_id)`. An IMMEDIATE transaction commits the created domain row, provenance edges and canonical serialized request/response together. Identical retries replay the original result; different payloads under the same key receive a typed `conflict`. Concurrent calls from distinct SQLite connections serialize correctly; daemon restart does not lose a committed result. Different trusted UIDs cannot retrieve each other's operation result by guessing the same ID. Operation rows are retained for **30 days after creation**, then opportunistically pruned on subsequent keyed writes: retries after expiration may create a second object.

Example (JSON command arguments; core replaces `created_by` with the trusted actor):

```json
{"name":"Plan","operation_id":"my-request-123"}
```

The registry stores serialized request arguments as well as original response payloads in the local state database for 30 days. Treat this database as sensitive persistent data: back up and protect it appropriately. A future design may substitute a cryptographic payload fingerprint to reduce duplicated sensitive text. An operation identity is a transaction-level replay guarantee **only for those SQLite create commands**; other metadata mutations, file writes, shell execution, PTY spawn, external API calls, generation activation, and deployments still need separate outcome/retry semantics.

No unsupported, silent, automatic retries should be introduced into agenticctl or autonomous agents. Do not claim general exactly-once delivery, failover, seamless process recovery or indefinite retention.

## Externally effective `session.spawn` (PR #156)

Unlike a metadata-only Session create, spawning a shell is an external process
effect. For `session.spawn`, an optional `operation_id` is now reserved with
an immutable Session ID in an IMMEDIATE SQLite transaction **before**
`Command::spawn` executes. The reservation actor comes from the owner of the
kernel-owned single-user runtime directory, rather than JSON `created_by`.
The key is bound to normalized `shell`, `created_by`, and `flow_id` arguments.
The key's reservation is **not pruned after 30 days** because forgetting a
potentially live external effect would make a late retry dangerous.

The first request may create a real shell and return its Session ID and control
token as before. A duplicate key (even after a supervisor restart) returns
typed `unavailable` without launching anything. Changed spawn arguments with
the same key return typed `conflict`. This is **at-most-once admission** for a
key, not at-least-once execution: a failed launch can leave the key reserved,
and a response timeout always means its effect is unknown.

A read-only `session.spawn.lookup` IPC command takes
`{"operation_id":"my-pty-request"}`. It returns the reserved `session_id`,
whether a Session row exists, and whether this daemon currently has a live
guardian. It never adopts a numeric PID, mints a control token or retries the
process. When ownership cannot be proven, `external_outcome: "unknown"` is
returned, including after a restart.

On startup the daemon scans all external reservations (including reservations
whose process started but whose Session row was not saved). With genuine
delegated cgroup-v2 authority it kills exact Session-ID-scoped stale cgroups,
never a bare PID that could have been reused. If cgroup recovery cannot be
proved, this is **not** an assertion that the old shell exited: remain in
unknown-outcome state and do not automatically retry. Development mode lacks
the production cgroup guarantee.

Client policy: retain a stable operation ID, inspect `session.spawn.lookup`
after an ambiguous response, and do **not** submit the same effect again
under a new key until it has been explicitly reconciled. No guarantee is made
about adopting old file descriptors or replaying capabilities across crashes.

### `session.spawn` idempotency policy (#74)

This is the single, precise contract. It reuses the durable operation-key
registry (`ipc_operations`, migration 0005) and its key rules; it adds no
second idempotency model.

1. **Key and binding.** The key is `(trusted actor, "session.spawn",
   operation_id)`, with the same `operation_id` syntax as the SQLite create
   commands. It is bound to the normalized `shell`, `created_by` and
   `flow_id`. The actor is the kernel-attested owner of the runtime
   directory, never a request field.
2. **At-most-once admission, not replay.** The first request reserves the key
   and an immutable Session ID in an IMMEDIATE transaction **before**
   `Command::spawn`. A repeated key never launches anything, never replays the
   first response and never re-issues its control token. It returns
   `unavailable` ("prior attempt has an unknown external outcome"); this
   holds after a restart too. Changed arguments return `conflict`. Unlike
   SQLite create keys, which replay a stored result, a spawn key cannot replay,
   because its result is a live process and a secret token.
3. **No expiry.** Spawn keys are never pruned: forgetting one could let a late
   retry start a second shell.
4. **Reconciliation.** `session.spawn.lookup` reports `session_recorded`,
   `guardian_supervised` and `external_outcome` (`supervised` or `unknown`),
   always with `retry_safe: false`. A client that sees `unknown` must treat
   the effect as possibly having happened, and may start a new Session only
   under a **new** key, as an explicit decision.
5. **Unkeyed spawns** stay legacy and non-idempotent: a timeout means outcome
   unknown, and nothing can be looked up. They are still recorded durably
   before launch (`session.spawn.unkeyed`, not a retry identity). The record
   is released when the Session row commits and cleared by restart recovery,
   so the cgroup of every launch can be found.
6. **Crash recovery.** On startup with delegated cgroup v2, `flowerlyd` kills
   the exact `pty-<Session ID>` cgroup of every recorded launch and of every
   PTY Session row, marks RUNNING PTY Sessions `suspended`/`detached`, and
   removes empty `management-*` groups left by earlier instances. It never
   signals a bare PID and never adopts an old guardian. Without delegation,
   spawning is disabled and recovery cannot prove cleanup; records are kept.

The kernel-level evidence is `scripts/test-pty-daemon-recovery.py`. It runs the
real `flowerlyd` in a `Delegate=yes` unit, with the shell under a distinct UID,
in the Arch guest. It covers:

- `flowerlyd` SIGKILL with keyed and unkeyed Sessions and setsid descendants;
- SIGKILL between guardian launch and the Session row, with the insert held
  behind a real SQLite write lock;
- total supervision loss: watcher, guardian and daemon SIGKILLed together;
- concurrent duplicate-key spawns, terminate and restart.

Before #74, the last combination with no key leaked a live setsid process in a
cgroup that no record named. The suite reproduced that leak and the empty
`management-*` groups that accumulated on every restart.

## Required evidence before closing #63

- A deterministic test makes a handler continue and commit after the caller has timed out, using a real durable SQLite database, then demonstrates a naive retry's duplicate effect.
- Opt-in create-operation key/reconciliation semantics must pass simultaneous retry, daemon restart, input mismatch and different trusted-actor tests; distinguish storage unit coverage from real-service enforcement.
- Validate no automatic retry of non-idempotent commands in CLI/agents; document explicit acknowledgement of unknown outcomes.
- Record measured timing and IDs, including a failure-mode test where worker cancellation and SQLite commit status differ.

## Contention and liveness limits

SQLite connections use a finite writer busy timeout. A separate process holding `BEGIN IMMEDIATE` can prevent independent writers from committing and temporarily occupy synchronous service Store mutexes. Ping must remain available; Status may correctly report `ready: false` while its Store mutex is busy. When the Store is free, Status reads all domain counts (`flow_count`, `goal_count`, `session_count`, `execution_count`, `generation_count`, `provenance_edge_count`, `model_count`, `profile_count`, `provider_count`, `active_workload_count`) in one SQLite statement, so they come from one snapshot. If that read fails, Status reports `ready: false`, `database: "error"` and the typed `error_code`, and leaves out the counts; it never reports `ready: true` with missing counts. This document does not guarantee distributed exactly-once execution.

## Storage error classification

`StorageError::ipc_error_code()` in `flowerly-core` is the single mapping from storage failures to IPC error codes. Every storage-backed command uses it. It matches on typed variants and SQLite result codes only; it never interprets error message text, so a diagnostic such as "database is locked" on a corrupt-database error stays `internal`.

| Storage failure | IPC code | Retry meaning |
| --- | --- | --- |
| SQLite `BUSY*` / `LOCKED*` (writer contention, snapshot, recovery, shared cache) | `unavailable` | Transient; the statement was rejected. Retry only per the client policy above. |
| `UniqueViolation`, `OperationConflict`, SQLite `CONSTRAINT_UNIQUE` / `CONSTRAINT_PRIMARYKEY` | `conflict` | Not transient; the request collides with existing state. |
| `ForeignKeyViolation`, `InvalidOperation`, SQLite `CONSTRAINT_FOREIGNKEY` | `invalid_argument` | Not transient; a referenced record is missing or the request is malformed. |
| `NotFound` | `not_found` | Not transient. |
| SQLite `CORRUPT`, `NOTADB`, `READONLY`, `FULL`, `IOERR`, `CANTOPEN`, `MISMATCH`, `CONSTRAINT_CHECK`, `CONSTRAINT_NOTNULL`, `CONSTRAINT_TRIGGER`, generic `CONSTRAINT`; row decode, serialization and I/O errors | `internal` | Never reported as transient; these indicate a defect, a damaged database or a host failure that retries cannot fix. |

Audit of storage-backed commands on `release/2` (issue #62, 2026-10-24):

| Service area | Commands | Storage error path |
| --- | --- | --- |
| Flow | `flow.create`, `flow.list`, `flow.activate`, `flow.archive` | Pre-validation and handlers use the shared mapping. |
| Goal | `goal.create`, `goal.child`, `goal.show`, `goal.list`, `goal.complete`, `goal.abandon` | Pre-validation and handlers use the shared mapping. |
| Session | `session.create`, `session.list`, `session.terminate`, PTY lifecycle persistence | Handlers and the PTY supervisor use the shared mapping. |
| Execution | `execution.create`, `execution.show`, `execution.list` | Shared mapping. |
| Generation | All Generation commands | Shared mapping; earlier generation-only extended-code handling now applies everywhere. |
| Provenance | `provenance.add`, `provenance.from`, `provenance.to` | Shared mapping. |
| Compute | Catalog, profiles, admission, inference ledger | Shared mapping. Before this audit, compute did not type `UniqueViolation` or `ForeignKeyViolation` and reported both as `internal`. |

Before this audit, six of the seven per-service copies reported raw SQLite UNIQUE/PRIMARY KEY and FOREIGN KEY failures as `internal`. Only Generation typed them.

Regression evidence is in the `flowerly-core` storage unit tests. They cover every error family, message-text spoofing, and real SQLite failures: a BUSY from a second connection holding `BEGIN IMMEDIATE` followed by recovery once the lock is released, a non-database file, and UNIQUE/FK/CHECK constraint failures. The six-service real-process writer-lock test from PRs #66–#67 still covers end-to-end `unavailable` and recovery.

Per-request elapsed time is logged as described above; lock-wait time inside SQLite is not measured separately and appears as `handler_ms`. Argument typing in the legacy fallback handlers is tracked under #53. Those fallback handlers can only be reached after the typed pre-validation that runs under the same Store lock.
