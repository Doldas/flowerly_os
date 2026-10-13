# Linux PTY guardian integration — P0 #50 draft

**Status: draft candidate, not release-certified persistent Sessions.**
RFC 0003 remains proposed, not an accepted ADR.

The Linux guardian in scripts/pty-guardian.py is embedded into sessiond. On
explicit session.spawn the daemon starts a separate guardian, which owns the
PTY master, allowlisted interactive shell child, process group, socket and a
64 KiB bounded output replay window. Unlike legacy session.create, a RUNNING
Session record is persisted only after a real process started.

The supervisor supports session.spawn, session.attach, session.read,
session.detach, session.write, session.resize, session.status and supervised
session.terminate. It verifies the kernel-attested creator injected by core
IPC, uses SO_PEERCRED at the guardian socket, and caches the Session actor for
responsive PTY I/O without SQLite mutex waits. A 16-session guardian limit,
4096-byte writes and two-second guardian socket timeouts constrain resources.
Owned socket inodes are checked before unlink; an occupied socket is never
replaced. The original child process group receives TERM then bounded KILL.

## Executable evidence

- scripts/test-pty-guardian.py runs a real separate Linux shell and checks
  stable PID, disconnected client/reconnect, replay, resize, and termination.
- scripts/test-sessiond-pty-flow.py runs real flowd and sessiond, persists
  Flows A/B, starts a real shell in A, switches to B and back to A, reattaches
  to the *same Session ID and still-running PID*, receives preserved output
  and terminates the real process.
- An extended gate checks SQLite writer-lock PTY responsiveness, rejects
  unsupported shells, and checks natural shell exit persisted as Exited.
  These additional assertions need a fresh green CI run.

## Limitations preventing release acceptance

- Attach/read are cursor-based request/reply snapshots, not a full-duplex
  stream multiplexer or multi-client write arbitration.
- No PTY FD adoption after daemon restart: stale Session IDs fail closed
  as unavailable rather than spawning replacement processes.
- No independent per-Session capability or delegation token on same-UID
  Unix sockets, pidfd/cgroup descendant lifecycle guarantee, shell sandbox
  or complete memory/CPU/process limits.
- External spawn cannot atomically commit with SQLite; operation_id is
  rejected to avoid false exactly-once promises. A timed-out spawn may have
  executed and must be reconciled before retrying.
- Legacy metadata-only Session operations are not proof of process execution.
- Supervisor-crash, malicious peer, concurrent writer/terminate, target Arch
  VM exact-commit validation and independent release security review are
  outstanding. Keep #50/#31 and milestone 0.0.4 open.
