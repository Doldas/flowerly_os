# IPC mutation outcomes, retries and SQLite contention

Status: **implemented SQLite retries, externally effective PTY safety under review** (issues #62 and #63). The original durable SQLite create-key implementation was merged in PRs #66–#67; PR #156 adds separately scoped PTY guardrails.

## What current IPC responses guarantee

- Each request uses a correlated IPC v1 response ID and has a bounded network/queue/handler response deadline.
- The worker holding an IPC handler slot continues until its blocking SQLite work returns, even if the client has already received a timeout. A timeout does **not** prove a database transaction was rolled back.
- `unavailable` caused by SQLite BUSY/LOCKED means SQLite rejected the operation under lock contention. `unavailable` from an overall IPC deadline **can have an unknown mutation outcome**.
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

## Required evidence before closing #63

- A deterministic test makes a handler continue and commit after the caller has timed out, using a real durable SQLite database, then demonstrates a naive retry's duplicate effect.
- Opt-in create-operation key/reconciliation semantics must pass simultaneous retry, daemon restart, input mismatch and different trusted-actor tests; distinguish storage unit coverage from real-service enforcement.
- Validate no automatic retry of non-idempotent commands in CLI/agents; document explicit acknowledgement of unknown outcomes.
- Record measured timing and IDs, including a failure-mode test where worker cancellation and SQLite commit status differ.

## Contention and liveness limits

SQLite connections use a finite writer busy timeout. A separate process holding `BEGIN IMMEDIATE` can prevent independent writers from committing and temporarily occupy synchronous service Store mutexes. Ping must remain available; Status may correctly report `ready: false` while its Store mutex is busy. Typed `unavailable` for BUSY/LOCKED is being extended to all domain operations under #62; this document does not certify other daemons or guarantee distributed exactly-once execution.
