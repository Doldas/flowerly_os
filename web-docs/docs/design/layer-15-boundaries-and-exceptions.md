# Layer 1.5 responsibility and driver-import policy

**Status:** Proposed gate for #91 #92 #93 #94 #95, ADR-0018 Proposed. Review alongside [existing inventory](../reviews/0091-0082-boundary-storage-inventory.md).

## Existing code and trust ownership

| Area | Existing direct primitives | Future boundary |
| --- | --- | --- |
| `agentic-storage/src/lib.rs` | rusqlite connections, SQL, IMMEDIATE transactions and migrations | Concrete storage adapter; typed command/query interface |
| `agentic-ipc/src/lib.rs` | UnixListener/Stream, kernel `peer_cred`, socket mode/ownership and bounded admission | **Temporary permanent-by-design exception candidate:** peer authority remains inside trusted transport |
| `agentic-core/src/daemon.rs` | trusted actor created from kernel-provided peer UID | Remains security admission edge, not JSON caller-supplied actor |
| `sessiond/src/main.rs` | delegated cgroup v2 discovery, /proc/self/cgroup, cgroup.procs, cgroup.kill and restart cleanup | Host-adapter extraction **deferred** until #72/#74 authority is independently proven |
| `sessiond/src/pty.rs`, `scripts/pty-guardian.py` | guardian lifecycle, capability/token protocols, PTY ownership, cgroup supervision | Trusted composite boundary; no aesthetic split of authority and effect |
| Other domain services | `agentic-storage::Store` APIs; no direct rusqlite Cargo dependency in selected manifests | Move domain queries/commands to typed repository interfaces incrementally |

This is not an exhaustive per-line code-search certification. A machine-generated callsite/import inventory with exceptions is a #93 deliverable, tested both by negative fixtures and normal builds.

## Import policy staged rather than brittle

Phase A: baseline deny rules for **new** production `rusqlite` imports outside storage adapters and new raw cgroup control imports outside reviewed host/trusted PTY code. Record paths and exact allowed symbols; keep existing exceptions visible.
Phase B: after #83 typed storage contract and #92 authority audit, route allowed services through typed capabilities. Do not ban Unix socket operations in `agentic-ipc`; this is the kernel-authentication boundary.
Phase C: verify feature matrix and dependency graph, with CI failing on intentionally introduced forbidden imports. Never suppress security or release gates to obtain green checks.

Prohibited public API forms: `execute_sql(String)`, `fetch_state(String)` with arbitrary SQL, `kill_pid(i32)`, `set_cgroup_path(PathBuf)`, `map_sandbox_volume(host_path, sandbox_path)` without authorized rooted FDs/mount policy. An interface is not privilege separation.

## Host capability contract proposals

Caller requests **intent**, e.g. resource class and bounded limits; a trusted broker authorizes operation against a kernel-verified owner-bound `SessionAllocation` identity. Return an effect receipt only after kernel enforcement and readback succeeds; unsupported delegation, wrong namespace/UID/PID identity, wrong cgroup inode, missing controller or permission failures return typed errors. Never return `Ok(())` on no-op.

For telemetry return measured values, source/clock, unsupported fields and staleness; never fabricate values. For future mounts require allowlisted roots, FD/path-resolution proof, symlink and namespace checks and independent authorized broker operations. Do not implement privileged KVM/mount mappings in #92 until #95 review and real test infrastructure exist.

## PTY/IPC authority invariants

- Preserve kernel-derived peer identity, socket inode-aware cleanup, same-UID admission and authenticated internal guardian commands.
- Preserve root EUID interactive PTY refusal and required delegated cgroup behavior.
- Preserve one writer lease, bounded observers/replay/backpressure, no SQLite waits in PTY stream owner, uncertain spawn reconciliation.
- Refuse raw caller-controlled PID/cgroup/path authority; PID reuse and namespace changes require real kernel identity proof.
- Same-UID sibling cgroup migration, setsid descendant escape, guardian/sessiond SIGKILL and restart remain **unproven acceptance until positive delegated Arch runs**. Keep #50/#72-#74 open.

## Verification split

Mock adapters test domain behavior, denied calls and timeouts. Real Linux tests prove process, credential, cgroup, mount and filesystem effects and their refusal. Compare mock and real error contracts; only actual delegated Arch/KVM tests can establish kernel containment. All reports must record exact commit, kernel, UID, delegation layout, guest image and artifacts.
