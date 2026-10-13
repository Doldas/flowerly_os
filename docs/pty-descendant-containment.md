# Linux PTY descendant containment candidate (issue #72)

**Status: draft, not an escape-proof production sandbox.**

The `sessiond` guardian owns a PTY master and an interactive shell.
Interactive shell job control can place `sleep` or another child in a
**different process group** from its shell while retaining the same
terminal session. Killing only the original shell group, or using
`PR_SET_PDEATHSIG` on that shell alone, does not cover these jobs.

## Current mechanism

The Linux guardian starts an independent cleanup watcher before releasing
the shell for exec. The shell and watcher both receive a Linux parent-death
signal (using `prctl`); the shell receives SIGKILL and the watcher responds
to guardian death by enumerating same-UID Linux processes still in the
shell's terminal session.

The watcher uses `pidfd_open` and `pidfd_send_signal` so it does not send
signals to an unchecked numeric PID after reuse. It checks terminal session
identity before and after obtaining each pidfd. The watcher closes its copy
of the PTY master and exits after a bounded cleanup pass. The guardian
also caps each PTY read burst to 64 KiB, preventing noisy subprocess output
from starving normal socket controls indefinitely. The bounded replay ring
retains only its last 64 KiB and signals a truncated cursor.

The `scripts/test-pty-guardian.py` Linux black-box starts a real
interactive shell with a background `sleep`, kills the guardian using
SIGKILL, and asserts the background job no longer executes. It also tests
the direct shell's parent-death signal, same-UID/wrong-PID denial, bounded
output replay, terminal resize, and reconnect.

## Security boundaries and release blockers

- This is a **session-membership cleanup**, not comprehensive descendant
  containment. A child can deliberately detach via `setsid`, a different
  namespace, or another Linux execution mechanism and escape this scan.
- A watcher that itself dies unexpectedly cannot guarantee cleanup. The
  current mechanism cannot substitute for Linux cgroup-v2 lifetime
  containment and real process quotas.
- The guardian now refuses to start as Linux EUID 0: without a tested
  per-user credential-delegation layer, session.spawn must never launch
  a normal interactive root shell. Deploy sessiond under a non-root UID.
- A local process sharing the same UID remains within the same Linux
  authority domain; peer PID admission is not a delegated capability.
- The `/proc` scan and cleanup deadline are bounded, so a hostile workload
  could race or continue spawning. Proper cgroup kill and resource limits
  are prerequisites to claiming robust process-tree ownership.
- Reconnecting to a dead guardian does not recover its PTY FD; fail-closed
  Session state reconciliation is tracked separately by issue #74.
- Target Arch exact-commit VM and adversarial stress tests remain necessary.

This work improves #50 but does not satisfy all of #72, #50 or the
Flowerly 0.0.4 release gates by itself.
