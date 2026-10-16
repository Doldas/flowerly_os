# Security blocker #72: same-UID delegated cgroup migration

**Observed on real Arch guest:** `test-pty-cgroup.py` reported `PTY_MIGRATION_MANAGEMENT_ESCAPED:0` while a shell was inside the PTY cgroup. This is a **successful containment escape**, not a flaky test. Release acceptance remains **failed**; do not skip or invert the assertion.

## Immediate mitigation in this PR

`scripts/pty-guardian.py` now refuses a required-isolation spawn when the guardian's *same effective UID* can open the delegated parent's or `management/cgroup.procs` for writing. The probe opens without writing any PID; absence of the trusted management cgroup and unverifiable access also refuse spawn. This is intentionally conservative: environments with insecure `Delegate=yes` topology may lose PTY availability rather than receive a falsely secure shell.

**This does not prove that arbitrary same-UID siblings cannot be created later and does not pass the positive Arch escape test.** There is a TOCTOU threat in any permission-only probe. Treat this as containment hardening, not completion of #50/#72/#74.

## Required architecture correction, separate reviewed change

A cgroup writable by a shell's UID is not its security boundary. A *trusted* supervising identity must exclusively own cgroup creation/membership migrations and protect sibling and parent `cgroup.procs`; an untrusted interactive PTY must not have access to an FD, delegated subtree or credentials allowing it to change membership. This likely needs a reviewed privilege-separated broker or distinct UID/security confinement with kernel-enforced mediation. Merely chmodding a directory and still running the process under the same privileged/delegated UID is not enough. Never run the interactive PTY as root.

Before claiming fixed, provide an independently reviewable topology design, check ownership and delegation at kernel level, then rerun the **unchanged** malicious management/parent migration, setsid descendant, guardian-SIGKILL and restart acceptance in a fresh Arch guest on exact SHA. Record the UID, mount/cgroup inode and results. A fail-closed denial is safe but is **not** the required successful positive PTY acceptance.

## Status
Mitigation: **Implemented**, tests pending exact-SHA workflows. Containment: **Not accepted**. No schema, dependency, CI gate or #55 changes.
