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

## Writer ownership and restart contract (draft #75)

`session.spawn` returns an ephemeral random `control_token`, which is
not stored in `session.list` or durable SQLite data. `session.attach`,
`session.read`, and supervised `session.terminate` require that secret
in addition to a kernel-attested actor. A status inquiry without the
secret can observe liveness but has its terminal replay removed.

`session.attach` then returns a separate random `attachment_token`.
Only one active writer is allowed per Session. A caller with the control
secret may read without owning the writer lease, but `session.write`,
`session.resize` and `session.detach` require the active writer token. A conflicting attach or revoked token fails with a typed
conflict error. A detached token cannot be reused. Writer leases expire
after 60 seconds without a writer action; clients must reattach. This
is an **in-memory single-writer lease**, not full-duplex streaming.

On `sessiond` restart, persisted RUNNING local-PTY records with a real
PID become `SUSPENDED` / `DETACHED` instead of claiming the old shell
remains under supervision. In this contract SUSPENDED means **unavailable /
lost supervision**, not proven Linux process suspension or safe adoption.
A newly started daemon cannot attach to a historical PID. A live daemon
checks the owned guardian's actual `Child` handle and only reconciles
unexpected process death; a socket timeout alone is not exit proof.

The control secret is an **in-memory possession check**, not an isolated
Linux authority when both processes share the same UID. Same-UID
processes may have other means of obtaining each other's secrets;
this mechanism does not substitute for delegated user credentials.

The supervised-spawn response exposes `supervisor_pid` for diagnostics
and fault injection. It is not authorization or a process identity token.

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

## Lifecycle and input fault-isolation tranche

The PTY shell now starts with conservative kernel resource limits:
`RLIMIT_NOFILE=256`, `RLIMIT_FSIZE=128 MiB`, and disabled core dumps.
These apply to the shell and children inheriting them, not to the entire
Linux UID or to all escaped descendants; CPU/memory/process-count limits
still require delegated cgroup-v2 enforcement.

Authenticated guardian IPC is untrusted input for protocol safety.
Non-object JSON, malformed UTF-8/JSON, invalid cursors, malformed write
hex, connection reset, a partial request and a disconnected reader must not
terminate the owned PTY guardian. The 64 KiB output replay and 4096-byte
input bounds still apply. The tests exercise the real guardian and execute
a process inside the PTY to verify the kernel-enforced resource limits.

This is an intermediate hardening tranche, not a claim of comprehensive
descendant isolation or production streaming.

## Limitations preventing release acceptance

- Attach/read are cursor-based request/reply snapshots, not a full-duplex
  stream multiplexer. Draft #75 adds a leased exclusive writer but not
  continuous stream fanout, disconnect observation or all race proofs.
- No PTY FD adoption after daemon restart: stale Session IDs fail closed
  as unavailable rather than spawning replacement processes.
- A leased writer token is not a cross-UID delegated capability or an
  independent per-Session security principal. PID-bound guardian controls
  and same-UID admission do not provide sandbox isolation. Linux cgroups,
  descendant lifecycle guarantees and complete resource limits remain.
- External spawn cannot atomically commit with SQLite; operation_id is
  rejected to avoid false exactly-once promises. A timed-out spawn may have
  executed and must be reconciled before retrying.
- Legacy metadata-only Session operations are not proof of process execution.
- Draft #75 adds guardian crash/daemon restart state tests and writer
  revocation checks, but stronger failure/race tests, exact-commit Arch VM
  validation and independent release review remain outstanding. Keep
  #50/#31 and milestone 0.0.4 open.
