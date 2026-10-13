# Linux PTY guardian — candidate for sessiond integration

Status: **prototype subsystem on feature branch, not production Sessions**.
Issue #50, release acceptance #31. RFC 0003 is proposed, NOT accepted.

The Linux-only guardian in scripts/pty-guardian.py owns a real shell child,
PTY master FD, output replay window (64 KiB), terminal resize, and the child
process group. Each local connection is admitted using SO_PEERCRED UID. Every
request is bounded and a disconnected client does not kill the child. An owned
child receives TERM followed by KILL after one second; the guardian observes
waitpid rather than trusting a stored PID. It fails closed when the parent
dies, cleaning up its owned process group. A socket is never replaced during
startup, and only its recorded socket inode is removed on exit.

Verify with python3 scripts/test-pty-guardian.py on Linux. The test launches
an actual separate process, reconnects to the same PID, reads shell output,
resizes the terminal, and terminates the child.

**Still required before production:**

- Rust sessiond integration with trusted session-to-actor authorization,
  durable SQLite identity and Flow linkage; the guardian alone has no
  Flowerly SessionId and must not be exposed to arbitrary direct clients.
- Concurrent stream ownership and a defined single-writer policy; no
  external terminal client should have unsupervised access to its socket.
- Observed exit status persisted after child natural exit, crash/reattach
  reconciliation, and explicit handling of late commits for child spawn.
- Full Flow A → B → A process continuity with sessiond, plus resource quotas,
  failure injection, busy-lock tests, Arch VM acceptance and security review.

This tranche is intentionally not equivalent to the 0.0.4 release gate.
