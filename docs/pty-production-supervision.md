# PTY production supervision and release acceptance

## Deployment boundary

Production starts `sessiond` as an unprivileged dedicated Linux user
under systemd. `FLOWERLY_PTY_ISOLATION` defaults to `required`.
A systemd service must use `Delegate=yes`. When
`FLOWERLY_PTY_CGROUP_ROOT` is set, it must point to an existing,
explicitly delegated writable cgroup-v2 subtree owned by the
supervision service, never a path supplied by `session.spawn`.
When unset, `sessiond` discovers its own cgroup from
`/proc/self/cgroup` and uses that delegated unit scope.

On startup, `sessiond` attempts to provision the delegated scope:
it verifies the real cgroup-v2 filesystem, creates a separate management
leaf, moves itself into that leaf, and enables CPU, memory and process
controllers. Failed provisioning does not bring down unrelated Session
metadata or SQLite endpoints. Instead, `session.spawn` returns a typed
`unavailable` error and does not launch an uncontained shell. The daemon
never silently switches to development isolation after provisioning
fails, and never attempts to elevate privileges.

The CI logs the runner's actual UID. Hosted PR runners use their already
unprivileged runner identity for interactive PTY regressions. Trusted
root-owned release runners provision a dedicated `flowerly-pty-ci` Linux
service account without supplementary groups and run the guardian,
cgroup fail-closed and Flow A to B to A tests as that unprivileged UID.
Both paths use a clean test environment and require accessible release
binaries. A separate root-executed negative test verifies that the guardian
rejects root-owned shell startup before binding a control socket. Missing
account provisioning, workspace access or root refusal fails the CI job;
the guardian security policy is not relaxed. The trusted release-push
gate uses a delegated transient **system** service with
`User=flowerly-pty-ci` and `Delegate=yes`, rather than assuming a
root-runner account has a systemd user manager. A PR CI pass is not
proof of this release-only delegated kernel check.

The CI regression `scripts/test-sessiond-pty-unavailable.py` starts
an actual `sessiond` with an invalid delegation root. It verifies that
ordinary Session metadata can be created and listed while supervised
PTY spawning is denied.

Every supervised PTY creates a new `pty-<random>` cgroup in that subtree.
The guardian blocks the shell before exec, installs pids, memory and CPU
limits, writes the child PID to `cgroup.procs`, then releases the child.
The guardian and its parent-death watcher remain outside the child cgroup.
On intentional termination or guardian loss, `cgroup.kill` terminates
members, including ordinary grandchildren which call `setsid()`.
Any failure to create or configure the delegated cgroup fails the
shell launch instead of silently losing containment.

Required kernel interfaces are unified cgroup v2, `pids.max`,
`memory.max`, `cpu.max`, `cgroup.procs`, `cgroup.events` and
`cgroup.kill`. Limits are 64 tasks, 512 MiB memory and a CPU quota
of one core per shell; shell-inherited `RLIMIT_NOFILE`, `RLIMIT_FSIZE`
and core-dump limits provide additional defense.

The only compatibility switch is
`FLOWERLY_PTY_ISOLATION=development`, set by **local testing** outside
a delegated cgroup. It preserves older PTY smoke tests but provides
only same-terminal-session background-job cleanup and must not be used
as evidence of production descendant containment.

## Idle writer lease renewal

`session.heartbeat` accepts the active `attachment_token` and renews
the exclusive writer's 60-second lease. An idle frontend sends this
periodically instead of generating dummy terminal input. An expired,
detached or incorrect token cannot renew a lease, and the heartbeat
returns `lease_active` and `expires_in_ms` without exposing output.
The in-memory lease still disappears on daemon restart; callers must
recover a new Session explicitly rather than reusing stale authority.

## Session authority and lifecycle

The core IPC boundary uses Linux peer credentials for the creator actor.
The guardian accepts its control commands only from the parent daemon
PID and UID. Each spawned Session returns an ephemeral possession secret
for attach, read, follow and terminate; attach provides a separate
expiring exclusive writer lease for write, resize and detach.

`session.follow` is an authenticated bounded incremental cursor read
with a maximum 250-millisecond wait, 64 KiB replay and an explicit
overflow marker. Multiple callers can retain independent cursors, while
the daemon serializes mutations to the same Session. This is a bounded
long-poll transport, **not a perpetual push socket** or a remote
enterprise-delegation mechanism.

A periodic reconciliation task observes child exit through the guardian,
persists an Exited/Detached Session even without client polling, and
eventually reaps completed guardians. When the guardian itself dies,
the durable state records unavailable/lost supervision, never pretending
the original PTY FD survived a daemon restart. Old PIDs are not authority
for post-restart signaling. `session.spawn` deliberately rejects
idempotency operation IDs because creating an external process is not
transactionally atomic with SQLite.

## Threat boundaries

Cgroup v2 containment prevents process-session escape by `setsid()`,
but it is **not** a privilege boundary between same-UID actors with
write access to the parent cgroup hierarchy. A hostile child with
permission to rewrite its cgroup membership may escape unless systemd,
Linux credentials or another independent kernel boundary forbids that
operation. Production systemd deployment must verify effective
delegation and ensure shell workload credentials cannot move themselves
out of their delegated subtree. A cgroup subtree writable by the
interactive shell UID does not satisfy that adversarial requirement.

The guardian service also does not provide general seccomp, mount,
network namespace, or file-system policy sandboxing. Cross-UID
delegation remains a separate authorization design gate.

## Verification

`python3 scripts/test-pty-cgroup.py` always tests production
fail-closed behavior with no delegated root configured. With
`FLOWERLY_PTY_CGROUP_ROOT` set to an actual delegated cgroup v2
subtree, it additionally spawns a child which calls `setsid()`,
kills its guardian with `SIGKILL`, and verifies that the direct shell
and detached descendant both stop. If the variable is absent, the
test explicitly prints that positive kernel containment is
**not verified**.

`python3 scripts/test-pty-guardian.py` and
`python3 scripts/test-sessiond-pty-flow.py target/release` run
their normal interactive smoke tests in explicit development mode.
The production release gate must additionally run the real cgroup
positive test against the exact target commit inside the intended
Arch/systemd guest.

## Arch guest invocation

In a booted Arch/systemd guest where the intended unprivileged
Session account has an active user manager, invoke:

```bash
bash scripts/arch-pty-cgroup-acceptance.sh
```

The script creates a delegated transient systemd user scope, discovers
its cgroup-v2 root, moves its own runner into a separate management
leaf, enables CPU, memory and process controllers, and then runs the
real `setsid()` guardian-crash test. On trusted root-owned GitHub runners,
`bash scripts/ci-pty-user.sh delegated` instead launches a transient
systemd system service under the dedicated unprivileged PTY account;
this path requires systemd system-manager delegation and fails closed
if the delegated cgroup is unavailable. It stops on missing kernel
delegation or any unexpected surviving descendant. Run against the
**exact commit** proposed for release, and archive the guest image,
kernel, systemd unit configuration, workflow SHA, console and test
output as review evidence.

When a `sessiond` restart sees a durable PTY Session and a matching
generated cgroup name, it writes `cgroup.kill` and waits for that
cgroup to disappear before marking historical records unavailable.
The new guard rejects legacy metadata-only `session.terminate` for
a supervised PTY whose guardian is no longer owned.

## Current trusted-runner deployment blocker

The trusted release CI run on commit `b77eaa0cea45f0ae589f0202389f77b00465ec95`
confirmed EUID 0 for the runner and a working dedicated unprivileged PTY
service identity (EUID 997). Root refusal, real guardian ownership, the
development-mode Flow A to B to A acceptance, and required-cgroup fail-close
passed. The release-only delegated positive gate failed because the
runner cannot contact a systemd system manager: `System has not been booted
with systemd as init system (PID 1)`. That is an infrastructure blocker,
not evidence of valid delegation. Do not change the gate to skip, fall back
to development isolation, or declare release CI green on that basis.

The delegated kernel test now additionally executes an interactive
same-credential subprocess that attempts to write its own PID to the
delegated **parent** `cgroup.procs`. It requires an explicit permission
denial (EPERM/EACCES). A successful migration or unrelated error fails
the acceptance test. Even a passing direct-migration probe alone does
not prove namespace escape, cross-UID boundaries or the broader malicious
same-UID threat model; those require additional independent isolation
review and adversarial execution inside the intended Arch guest.

The official `tests/arch-kernel/guest-verify.sh` now executes these PTY
regressions **inside the booted and upgraded Arch guest**, including root
refusal as a negative test and the delegated positive cgroup test as a
systemd-managed unprivileged service. The guest report captures the
result at the exact exported commit SHA. A successful Rust build,
development-mode shell replay, or a hosted PR check cannot replace the
delegated production result. A failed migration-denial check makes the
Arch guest acceptance red by design.

The hosted PR job `scripts/test-pty-adversarial.py` independently
checks slow historical readers against the 64 KiB cap, concurrent
incremental followers, abandoned client sockets, writer-token revocation,
concurrent terminal mutation/termination, and abrupt `sessiond` SIGKILL.
It verifies original kernel process identities cease, stale durable
Session capabilities do not resurrect ownership, and a fresh Session
can launch after restart. These tests run outside delegated cgroups and
therefore are not substitutes for the production cgroup crash matrix.

## Open acceptance evidence

Before marking issue #50 complete, require the positive delegated
cgroup test on the exact release commit, malicious same-UID cgroup
escape tests with separate Linux credentials, Arch VM acceptance,
independent stream subscriber/backpressure review, and operational
approval of the user/process privilege boundary. Passing Ubuntu
development-mode tests alone is insufficient.
