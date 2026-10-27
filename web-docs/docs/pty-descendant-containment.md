# Linux PTY descendant containment candidate (issue #72)

**Status:** the delegated cgroup-v2 mode has adversarial kernel-level evidence
(see [Delegated cgroup v2: adversarial evidence](#delegated-cgroup-v2-adversarial-evidence-72)).
The development-mode session scan described first is **not** a sandbox.

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

## Delegated cgroup v2: adversarial evidence (#72)

Production isolation (`FLOWERLY_PTY_ISOLATION=required`, the default) puts
every Session's shell in its own `pty-<Session ID>` cgroup under the
daemon's delegated root. The limits are `pids.max` 64, `memory.max` 512 MiB
and `cpu.max` of one CPU. The shell runs under a **distinct, capability-free
UID**. Terminate, guardian death and restart recovery all use the
kernel's `cgroup.kill`, never a numeric PID.

`scripts/test-pty-containment-adversarial.py` runs the real `flowerlyd` in a
`Delegate=yes` unit in the Arch guest (`guest-verify.sh`) and asserts kernel
state only.

| Case | Verified behavior |
| --- | --- |
| Namespace escape | From the shell, writing its PID to the parent, `management-*` and host `cgroup.procs` gives `EACCES`. The same holds from a new user namespace (UID 0 inside). In a user+mount+cgroup namespace with its own cgroup2 mount, the view is the Session cgroup itself: listing, `mkdir` and `cgroup.procs` give `EACCES`. `setns` into PID 1's cgroup, mount and user namespaces gives `EACCES`. The shell's cgroup is unchanged afterwards, and terminate kills processes inside user namespaces. |
| Double-close, PID reuse | A second `detach` or a concurrent double `terminate` gets a typed error, never `internal`. A decoy created with the exited shell's **exact PID**, by a root helper writing `ns_last_pid`, survives stale `terminate`/`heartbeat`, the reconcile loop, `flowerlyd` SIGKILL and restart recovery. |
| Resource exhaustion | A fork bomb stops at `pids.max`, and a memory hog is OOM-killed inside its own cgroup (`memory.events oom_kill`). Four CPU spinners are throttled (`cpu.stat nr_throttled`), and a `yes` flood keeps replay at 64 KiB and the daemon's RSS small. Throughout, `flowerlyd` answers immediately and a bystander Session keeps working. |
| Zombies and orphans | Terminate kills double-forked and setsid orphans and an unreaped zombie. After a **natural** shell exit, background jobs stay contained for the 20 s replay window, then `flowerlyd` ends the guardian and its `cgroup.kill` removes them with no client action. No zombie guardian remains. |
| Fail closed | If the delegated root loses the `pids` controller, `session.spawn` returns `unavailable` before any shell-UID process starts and leaves no cgroup. Without delegation, spawning is disabled at startup (`PTY delegation unavailable; spawning disabled`). |

Crash and restart recovery, including total loss of the watcher, guardian and
daemon, is covered by `scripts/test-pty-daemon-recovery.py` (#74).

Limits that remain true:

- **The shipped 0.0.4 `flowerlyd.service` has no `Delegate=` and no
  `CAP_SETUID`/`CAP_SETGID`, so PTY spawning is disabled in the default
  installation** (fail closed). The delegated mode is verified only in the
  acceptance harness (`arch-pty-cgroup-acceptance.sh --system`), which runs
  a `Delegate=yes` unit with a distinct shell UID. A supported unit
  configuration for it is future work.
- The manager and its guardians share one UID. The boundary that is proven
  is shell-UID → cgroup, not manager → guardian.
- Unprivileged user namespaces were enabled in the tested guest. The results
  show they do not widen cgroup access; they are not otherwise restricted.
- `FLOWERLY_PTY_ISOLATION=development` has none of these guarantees and is
  never inherited by the shipped unit (#240).
