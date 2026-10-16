# #72 – kernel-enforced separation of PTY shell credentials

## Implementation in this branch (not yet Accepted)

The Arch production acceptance now provisions **two distinct non-root UIDs**: a trusted delegated-cgroup/guardian identity (`flowerly-pty-ci`) and the untrusted shell identity (`flowerly-pty-shell-ci`). Only the trusted manager unit receives `CAP_SETUID` and `CAP_SETGID` as ambient capabilities, strictly for dropping shell credentials. It continues to own the delegated cgroup root and its kernel-owned membership controls.

`SessionCgroup` in required mode checks the *shell UID's actual kernel access* to parent and management `cgroup.procs` by forking a probe, dropping identity/capabilities and attempting **non-writing** opens. It refuses a writable or unverifiable destination. No caller-selected PID, cgroup root, or arbitrary migration path is accepted through the PTY socket. The trusted guardian creates the Session cgroup and moves a still-blocked `forkpty()` child there before releasing it.

After release, the child drops supplementary groups, real/effective/saved GID and UID, sets `PR_SET_NO_NEW_PRIVS`, explicitly clears effective/permitted/inheritable Linux capabilities, and only then executes `/bin/sh` or `/bin/bash`. A failure exits before shell execution. The untrusted child inherits no live cgroup control FD across `exec` (`O_CLOEXEC`/Python non-inheritable FDs). The Arch acceptance now reads `/proc/<shell-pid>/status` to verify all four UID fields, empty supplementary groups, and zero `CapEff`, `CapPrm`, `CapAmb`.

The existing same-UID guard stays in place when `FLOWERLY_PTY_SHELL_UID` is absent. The prior untrusted single-UID `Delegate=yes` deployment is **not silently accepted**.

## Risk, scope and remaining release conditions

This is an **integration candidate**, not proof of a full production-ready security boundary. The trusted manager/guardian has significant capabilities; compromise of its IPC parser would expose the elevated manager, even though the shell drops capabilities. The separate `pty-cgroup-broker.py` prototype is not wired into this route. This design uses the trusted guardian itself as the manager/broker rather than enabling the unreviewed prototype. Further hardening should move capability-bearing launch operations to a smaller auditable, socket-activated manager.

Cgroup kill and guardian-crash watcher behavior remains the kernel-supervised guardian implementation, unchanged. Watcher SIGKILL/restart, cgroup migration, `setsid` descendants and exact-SHA Arch tests must actually run and pass before acceptance. The systemd runtime and capability provisioning must be inspected in the real VM (including capabilities available after transient unit delegation). No CI assertions, release gate, root refusal or issue #55 are modified.

**Status**: Implemented in draft, testing and security review pending; #72 **not Accepted** until successful Arch verification.
