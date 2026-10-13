# 0016: Supervised Linux PTY Session Ownership

Date: 2026-10-13  
Status: Proposed (review required; does not replace RFC 0003 yet)

## Context

0.0.4 must allow a user to detach from a real shell, switch Flows and
reattach without losing its child process or Session identity. Persisting
a Session row or PID alone is not process supervision. The IPC services
also cannot hold a SQLite mutex across terminal reads and writes.

## Proposed decision

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
- `session.attach` returns a random, in-memory writer lease token.
  Only one unexpired writer exists. `session.write`, `session.resize`
  and `session.detach` require it. Other authenticated callers can
  use `session.read/status` as cursor-based read-only observers.
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
  SIGKILL. A separate same-terminal-session watcher attempts to clean
  normal job-control background processes using pidfds, but this is
  **not** comprehensive descendant containment.

## Alternatives and trade-offs

A transient frontend-owned PTY would kill background shells on client
disconnect. A single blocking SQLite-backed server loop would stall
output during database lock contention. A standalone unprivileged
guardian trades extra child processes for independent FD lifetime and
fault isolation. Full cgroup-v2/systemd delegated scopes remain necessary
to contain children deliberately escaping the terminal session; kernel
pidfds prevent signaling recycled numeric PIDs but do not substitute for
cgroup ancestry or resource accounting.

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
- [ ] Merge and verify PR #75 owner-lease and restart/crash reconciliation
      against the exact release commit.
- [ ] Merge and verify PR #76 bounded watchdog/background-process test
      against the exact release commit.
- [ ] Prove **cgroup-equivalent** containment of detached descendants and
      enforce process, memory, CPU and FD quotas under hostile workloads.
- [ ] Prove deliberate multi-client races, slow readers and failure
      recovery/cleanup; audit same-UID authority and token leakage.
- [ ] Pass complete Arch guest/VM validation at the intended source SHA
      and obtain explicit owner review of the decision.

Do not change the status to Accepted, close issue #50 or certify 0.0.4
until those conditions pass. Related issues: #31, #50, #72, #73, #74.
