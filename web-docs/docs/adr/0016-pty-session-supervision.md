# 0016: Supervised Linux PTY Session Ownership

Date: 2026-10-13  
Status: Accepted (owner decision, 2026-10-25, #73). The unchecked
verification items below remain release gates for #50, #72 and 0.0.4;
acceptance of the design does not certify the release.

## Context

0.0.4 must allow a user to detach from a real shell, switch Flows and
reattach without losing its child process or Session identity. Persisting
a Session row or PID alone is not process supervision. The IPC services
also cannot hold a SQLite mutex across terminal reads and writes.

## Decision

Each Linux `sessiond` process owns an in-memory registry of supervised
Sessions. A separate bounded **guardian subprocess** per Session owns the
PTY master, controlling-terminal shell, direct child PID and replay ring.
This process boundary is justified by independent process ownership,
fault response and PTY FD lifetime, consistent with ADR-0013.

- The core IPC server derives `created_by` from the kernel peer UID, not
  from caller-supplied JSON. The guardian verifies its controlling
  `sessiond` UID **and PID** through `SO_PEERCRED` on every request.
- `session.spawn` only accepts allowlisted shells; it persists RUNNING
  after the real child is started. No caller PID or external-operation
  idempotency key is accepted.
- `session.spawn` returns a random, ephemeral control token not present
  in `session.list`. It is required to attach, read terminal output or
  terminate the owned shell. Without it, status exposes liveness only,
  not replay bytes. This is possession-based authorization within a
  shared Linux UID, **not independent credential isolation**.
- `session.attach` returns a separate in-memory writer lease token.
  Only one unexpired writer exists. `session.write`, `session.resize`
  and `session.detach` require the active writer token. A caller with
  the Session control token may read without owning the writer lease.
- Writer leases expire after 60 seconds of inactivity. There is no
  claim of a durable attachment across `sessiond` restarts.
- PTY output is bounded to a 64 KiB replay window. Cursors are monotonic
  byte offsets; readers detect replay truncation. Writes are limited to
  4096 bytes, guardian I/O deadlines to two seconds, and supervised
  entries to 16 per daemon.
- PTY I/O does not hold the SQLite mutex. Flow→Session identity and
  actual shell exit are persisted independently of transient clients.
- Direct child exit is observed via `waitpid`; terminates address an
  actually owned child rather than a stored PID. An exited guardian
  makes the Session unavailable and detached, **not reattached**.
  On restart, historical RUNNING PTY records are reconciled to
  SUSPENDED/DETACHED. SUSPENDED here denotes *lost supervision*, not
  proof of OS process suspension or safe adoption.
- Linux parent-death signaling protects the direct shell on guardian
  SIGKILL. A separate watcher is outside the child cgroup. The proposed
  production mode requires a delegated cgroup-v2 hierarchy, moves the
  shell into its own cgroup before exec, applies pids/memory/CPU quotas,
  and uses kernel `cgroup.kill` on guardian loss and termination.
  Session cgroup names derive from generated Session IDs, allowing
  restart cleanup without PID-based signaling.
- `session.follow` extends cursor-based reads with a bounded
  incremental wait of at most 250 milliseconds. Independent readers
  retain their own cursors, and writer actions are serialized with
  lifecycle operations without holding the SQLite mutex.
- A periodic supervisor reconciliation task persists natural shell exit
  even without a client polling status and later reaps expired guardians.

## Alternatives and trade-offs

A transient frontend-owned PTY would kill background shells on client
disconnect. A single blocking SQLite-backed server loop would stall
output during database lock contention. A standalone unprivileged
guardian trades extra child processes for independent FD lifetime and
fault isolation. Delegated systemd cgroup-v2 scopes supply kernel
descendant accounting and cleanup in the proposed production mode.
Session-mode pidfd scanning remains explicit development compatibility
and must never be presented as escape-proof confinement.

Full-duplex streaming sockets, automatic client-disconnect observation,
multi-subscriber backpressure, remote delegation tokens, and PTY FD
handoff across daemon restarts are **not implemented** by the current
pull-based contract. The initial 0.0.4 policy is explicitly fail-closed
on restart rather than falsely promising transparent adoption.

## Verification before acceptance

- [x] Real Linux `sessiond` Flow A→B→A same Session ID, same live shell
      PID, background process continuity, resize and bounded replay
      (merged PR #69).
- [x] Guardian peer UID+parent PID verification (merged PR #70).
- [x] Direct shell parent-death test on guardian SIGKILL (merged PR #71).
- [x] PR #75 owner-lease and restart/crash reconciliation merged, with
      required PR checks green.
- [x] PR #76 bounded watchdog/background-process test merged, with
      required PR checks green.
- [x] PR #77 malformed IPC and child-limit hardening merged with its
      Linux regression coverage.
- [x] PR #78 production delegated cgroup supervision, authenticated
      follow and unattended-exit reconciliation merged.
- [ ] Prove **cgroup-equivalent** containment of detached descendants and
      enforce process, memory, CPU and FD quotas under hostile workloads.
- [x] Prove deliberate multi-client races, slow readers and failure
      recovery/cleanup; audit same-UID authority and token leakage
      (long-poll contract #215, writer-lease expiry tests #219, same-UID
      limits in `pty-production-supervision.md`).
- [x] Explicit owner review of the decision (accepted 2026-10-25, #73).
- [ ] Pass complete Arch guest/VM validation at the intended source SHA.

Do not close issue #50 or certify 0.0.4 until the remaining unchecked
conditions pass. Related issues: #31, #50, #72, #73, #74.
