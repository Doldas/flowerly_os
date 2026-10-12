# RFC 0003: Linux PTY supervision and reconnect contract

Status: **Proposed; NOT an accepted ADR or production implementation**  
Target: 0.0.4 (issue #50; acceptance gate #31)  
Date: 2026-10-12

## Why a design gate is necessary

`sessiond` currently persists Session metadata. `session.terminate` updates DB rows;
it **does not** terminate a supervised child. A real persistent Session must own an
actual Linux PTY master and a child process tree independently of any attached UI.
Blocking SQL handlers under `Arc<Mutex<Store>>` cannot be the streaming PTY hot path.

## Proposed ownership boundaries

1. One privileged-by-its-own-UID **session supervisor** owns each PTY master FD,
   child identity, process group and bounded output buffer. It may be a task in
   `sessiond` initially; a new daemon is justified only by measured fault/privilege
   isolation needs (ADR-0013). Child UID must not gain host root.
2. Fork a new session/process group, acquire controlling terminal on the PTY slave,
   connect stdio, and exec an allowlisted shell. Keep master in the supervisor,
   not in a transient frontend connection. Prefer a battle-tested PTY abstraction;
   unsafe syscalls must be contained and reviewed, not spread across handlers.
3. An attachment is a **revocable client stream**, not the Session runtime. Client
   disconnect detaches only its subscriber. Reconnecting by authenticated SessionId
   attaches to the **same supervisor and existing process**, never silently spawns a
   new shell. Define a single-writer policy and read-only observers explicitly.
4. Use bounded in-memory byte/ring buffer and per-attachment cursors. Document
   overflow and replay-window semantics; slow clients must not cause unbounded RAM
   or block child output indefinitely. Stream I/O tasks must not hold the SQLite
   mutex or a transaction while awaiting bytes or clients.
5. Persist the transition to RUNNING only after child creation succeeded; observe
   `waitpid`/equivalent to mark EXITED with factual exit status. Killing a process
   group requires proof it is still the **owned** child group (PID reuse hazards).
   Avoid treating a stored PID by itself as authority to send a signal.
6. On supervisor death, ownership of master and child must be explicit. Initial
   0.0.4 may **fail closed**: mark the Session unavailable and reconcile observed
   children, without promising seamless daemon-restart PTY adoption. Do not claim
   durable reattachment across supervisor restart until a separate master-FD handoff
   or guardian design passes tests.
7. Authorization: verify kernel peer credentials on each attach and administrative
   control operation; reject arbitrary SessionId access. Per-command capability/
   delegation decisions remain the unresolved authorization gate in #31.

## State model and operation contracts

- `session.spawn`: validates Flow and policy; starts child under supervision;
  returns stable SessionId only on success.
- `session.attach`: attaches to existing live Session; cannot create another child.
- `session.detach`: drops one attachment; Session stays RUNNING.
- `session.resize`: validates dimensions, applies `TIOCSWINSZ`, emits WINCH.
- `session.write`: authenticated writer delivers bounded bytes to PTY master.
- `session.terminate`: verified owned process group receives TERM, followed by
  bounded grace/optional KILL; report observed exit, not optimistic metadata.
- `session.status`: distinguishes running, exited, unavailable and whether any
  attachment is present. Unknown or already-exited sessions have typed errors.

**These are proposed future API operations, not claims about existing code.**

## Hard test gates before the production backend

- [ ] Real PTY: child is a session leader with controlling tty and a stable group.
- [ ] Spawn one shell, detach client, change Flow A to B, return and reattach with
      same SessionId, same child PID and continuing output.
- [ ] Client disconnect/slow-reader: no shell death and bounded memory.
- [ ] Resize propagates to the child; signal tests confirm only the owned group
      is affected; process exit/status is observed and durable.
- [ ] Crash/killed supervisor: no uncontrolled orphan, unexpected restart claims
      or PID-reuse signal; documented degradation/recovery.
- [ ] Concurrent attach and terminate; races are deterministic and bounded.
- [ ] SQLite writer-lock injection (#56): PTY I/O remains responsive; no DB mutex
      is held across stream awaits; IPC deadlines include queue delays.
- [ ] Run tests as an unprivileged user on Linux and in the target Arch VM at the
      exact intended release checkout.

## First experimental evidence

`scripts/pty-process-spike.py` is a standalone Linux stdlib experiment using
`forkpty` to test real process/terminal ownership, frontend detach and process
group termination without claiming a production Session API. It **does not** prove
supervisor restart/adoption, authorization, buffering limits, or Flow integration.

## Unresolved design questions

- Supervisor lives in `sessiond` or in a smaller guardian with FD handoff?
- Multi-client attach/write arbitration and output replay guarantee?
- Exit-status retention and recovery when the daemon dies before reap?
- cgroup v2 scope limits, environment sanitization, shell selection and per-user
  quotas? Exact command/permission model before any untrusted workload?
- Whether a separate data-stream Unix socket is needed alongside IPC v1.

Once spike evidence and threat-model review are available, record the final
choice as an **accepted numbered ADR**; this proposal is intentionally not one.
