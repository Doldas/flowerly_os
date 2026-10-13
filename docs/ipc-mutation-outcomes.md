# IPC mutation outcomes, retries and SQLite contention

Status: **current protocol limitation and P1 design gate** (issues #56, #62, #63). This is not an idempotency implementation.

## What current IPC responses guarantee

- Each request uses a correlated IPC v1 response ID and has a bounded network/queue/handler response deadline.
- The worker holding an IPC handler slot continues until its blocking SQLite work returns, even if the client has already received a timeout. A timeout does **not** prove a database transaction was rolled back.
- `unavailable` caused by SQLite BUSY/LOCKED means SQLite rejected the operation under lock contention. `unavailable` from an overall IPC deadline **can have an unknown mutation outcome**.
- Neither an IPC request ID nor a reconnect currently constitutes a durable exactly-once operation key. An automatically retried `flow.create`, `goal.create` or `session.create` can create another durable object.
- Read-after-timeout reconciliation is possible in some cases by listing or querying domain records, but names and goals are not guaranteed unique and that alone is **not** a correctness proof of idempotency.

## Safe interim client policy

1. Never interpret a timed-out mutation as "not applied".
2. Do not automatically retry a non-idempotent create based only on the timeout/unavailable response.
3. Report **outcome unknown** distinctly in client UX/logging and recommend explicit reconciliation where possible. For a critical or automated workflow, stop pending an authoritative result.
4. Include correlation ID, actor identity, command name, elapsed time and response classification in diagnostics without logging secrets or full sensitive payloads.
5. No agent may guess that a task is safe to repeat merely because it is conceptually a "retry".

## Proposed durable operation identity (not implemented)

Evaluate a backward-compatible optional operation key scoped to `(trusted actor, command domain, operation ID)` with a request-payload fingerprint and bounded retention. In one transaction, persist an operation outcome and its created object/side effect so a later request with the same identity returns the same outcome. Mismatched payload under the same key must conflict; different users must not retrieve one another's results. Define errors, in-progress ownership, orphan/restart recovery, retention/GC and concurrency races before implementation. Unknown-key legacy commands retain old behavior and are not retrospectively guaranteed idempotent.

The runtime must not claim atomic outcome recording for effects outside the SQLite transaction (e.g. spawning a PTY process, remote API calls, service restarts). Those need their own operation reconciliation/compensation semantics.

## Required evidence before closing #63

- A deterministic test makes a handler continue and commit after the caller has timed out, using a real durable SQLite database, then demonstrates a naive retry's duplicate effect.
- Implemented key/reconciliation semantics are verified under simultaneous retries, daemon restarts and different trusted actors.
- Validate no automatic retry of non-idempotent commands in CLI/agents; document explicit acknowledgement of unknown outcomes.
- Record measured timing and IDs, including a failure-mode test where worker cancellation and SQLite commit status differ.

## Contention and liveness limits

SQLite connections use a finite writer busy timeout. A separate process holding `BEGIN IMMEDIATE` can prevent independent writers from committing and temporarily occupy synchronous service Store mutexes. Ping must remain available; Status may correctly report `ready: false` while its Store mutex is busy. Typed `unavailable` for BUSY/LOCKED is being extended to all domain operations under #62; this document does not certify other daemons or guarantee distributed exactly-once execution.
