# #72 – privilege-separated cgroup broker: phase 1

**Implementation: foundational broker and negative tests. Production activation: BLOCKED.**

The current production Arch acceptance *correctly fails*: under `Delegate=yes`, the same UID can write the parent's or management cgroup's `cgroup.procs`. The existing guardian fail-closed guard must remain. Simply suppressing its exception or changing the test would reinstate a proven escape.

## What this change implements

- `scripts/pty-cgroup-broker.py` – a separately runnable non-root broker restricted to a dedicated delegated cgroup root. It refuses running as root and refuses when the authorized caller has the same effective UID as the broker.
- Unix-domain `SO_PEERCRED` validation on each request, strict JSON shape and length checks, fixed Session identity format, no arbitrary paths, no caller-supplied PID or arbitrary signals.
- `reserve`: create a named cgroup with fixed resource limits. `reap`: `cgroup.kill` and remove only an in-process owned Session. Both use directory file descriptors with `O_NOFOLLOW`.
- Negative protocol tests for other UID, unexpected fields, traversal names, PID injection and unowned reaping.
- No change to the existing guardian's execution path, Arch checks, CI security gates or PTY shell UID.

## What is deliberately NOT implemented

The broker **must not yet be enabled**. Its private mode-0600 socket is inaccessible to a separate-UID client by design, until a reviewed, access-controlled systemd socket-activation scheme is supplied. `reserve` does not place or launch any process; running a PTY under the new broker would be a **false security claim**. In-process Session ownership does not provide crash durability, and `reap` is not yet used by guardian recovery.

## Next reviewed phase (required before production)

1. Configure two distinct *non-root* users: trusted cgroup-owner and untrusted PTY shell; verify kernel-owned cgroup ownership and that the shell cannot write management/parent/sibling migration files.
2. Introduce a tightly scoped process-placement API with kernel-pinned process identity (pidfd, lineage and cross-UID authorization), avoiding PID reuse. Avoid capability leakage, and do not permit caller-selected migration destination.
3. Wire guardian startup atomically: reserve, securely launch as untrusted UID, place in Session cgroup before any shell instruction, then release. Fail closed on every error.
4. Implement durable recovery and cleanup after broker/guardian SIGKILL, including independent ownership proofs and eventual cgroup.kill.
5. Preserve and rerun **unchanged** positive and adversarial Arch acceptance: same-UID management and parent migration refusal, setsid descendant termination, guardian SIGKILL, daemon restart and resource limits on exact SHA.
6. Keep root-owned interactive PTY prohibited and #55 unchanged.

A broker which only prevents a permission escape by refusing service is a mitigation, not successful kernel containment. ADR-0016 and issue #72 should remain unaccepted until a positively functioning, adversarially verified separated system exists.
