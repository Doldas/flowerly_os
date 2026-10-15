# 0018: Strict Boundary Isolation Layer (Layer 1.5)

Date: 2026-10-14  
Status: Proposed — architecture and security review required  
Source: user-supplied *Architectural Specification: Strict Boundary Isolation Layer (Layer 1.5)*

## Context and the requested separation

Flowerly OS must keep Linux kernel/syscall/ioctl, `/proc`, `/sys`,
cgroups v2, and third-party SQLite/libSQL driver mechanics out of the
higher cognitive control planes. The submitted design defines:

- **Layer 1 — Host Primitives:** Linux kernel and external engines:
  `rusqlite` (SQLite bindings) and optional libSQL/Turso.
- **Layer 1.5 — Structural Translation Boundary (Distro Facades):**
  implementation adapters translating typed operations to concrete
  kernel/engine operations and execution streams.
- **Layers 2–5 — Flowerly Core:** `agenticd`, `flowd`, `sessiond`,
  `goald`, `agenticctl` and future services, consuming explicitly
  defined distro-facing Rust contracts rather than driver-specific APIs.

The specification proposes pure trait definitions without direct storage or
Linux driver imports in their interface modules. Storage adapters are
envisioned under `crates/agentic-storage/src/engines.rs`, and a host
abstraction under `crates/agentic-core/src/host.rs` with an implementation
under `crates/agentic-core/src/drivers/linux.rs`. Those paths are
**proposed**, not current filesystem locations.

## Storage facade contract

The source proposes `DistroDataFacade` with asynchronous
`fetch_state(query, binds)`, `commit_state(TransactionDelta)`, and
`dispatch_sync()`; its representative types are `DatabaseMode`,
`TransactionDelta` and `RecordSet`. Its examples select a local
`LocalSqliteEngine` behind `sqlite-native` and
`ClusterTursoEngine` behind `turso-mesh`.

These are **illustrative interfaces**, not executable implementations:
the submitted adapters use `todo!()`. A literal universal SQL interface
would leak untrusted SQL into the facade and weaken typed domain authorization.
Before implementation, reconcile these names with ADR-0017's
`StorageMode` (Standalone, Cluster, CoreOnly) and domain-specific
`SystemStateRepository`, `MemoryRepository`, `EmbeddingIndex`,
`ReplicationController`. The source's two `DatabaseMode` values
represent backend selection; they do **not** remove CoreOnly or determine
whether a memory database should be opened.

The actual accepted architecture remains one shared SQLite `state.db`
under ADR-0011. Neither an adapter nor a trait is permission to drop
foreign-key/cross-domain ACID guarantees. Never expose driver handles or
arbitrary SQL to agents. Retain versioned migrations, provenance and durable
commit semantics; do not block async executors with synchronous SQLite locks.
Test libSQL/Turso availability, feature and concurrency claims against
verified upstream versions before implementation.

## Linux host facade contract

The source proposes `LinuxSubsystemFacade` with
`restrict_resources(ProcessAllocation)`,
`gather_telemetry() -> HostTelemetry`, and
`map_sandbox_volume(host_path, sandbox_path)`. Its intended Linux adapter
maps these into cgroups-v2 files, `/proc` and future KVM
mount/volume interfaces.

The source's `NativeLinuxDriver` returns fabricated telemetry values,
unconditionally returns success for resource restriction and mapping,
and constructs a cgroup path from a numeric PID. Those snippets are
**nonfunctional sketches and unsafe as production contracts**.
Production methods must verify effect, authority, ownership and exact
kernel identity, or return a typed error; never succeed on a no-op.
Resource quota semantics should not be represented as a universally valid
CPU percentage without checking kernel controls and delegation.

## Security and layering constraints

Abstraction is **not privilege separation**. Existing PTY cgroup
supervision, parent-death cleanup, Linux peer credentials, FD/PID ownership
and Arch delegated acceptance must not be made less secure by hiding them
behind a generic facade. Drivers with privileged effects require independent
authorization and kernel-enforced authority boundaries; caller-selected
numeric PIDs, sandbox/host paths and cgroup target strings are not proofs
of ownership. Reject namespace migration, symlink traversal, PID reuse,
same-UID sibling-cgroup escape and out-of-scope mounts.

Keep IPC transport and trusted process supervision boundaries explicit.
No blanket relocation of `SO_PEERCRED`, guardian processes or systemd
operations solely to satisfy an aesthetic dependency rule. Crate import
rules should describe narrow reviewed exceptions, with exact remediation
plans, instead of claiming a presently nonexistent driver-free core.

## Compatibility and acceptance

- [#91](https://github.com/Doldas/flowerly_os/issues/91): decide boundary,
  audit current imports and document transition.
- [#83](https://github.com/Doldas/flowerly_os/issues/83): implement
  domain-typed storage trait boundaries, compatible with ADR-0017.
- [#92](https://github.com/Doldas/flowerly_os/issues/92): real typed host
  facade and Linux adapters.
- [#93](https://github.com/Doldas/flowerly_os/issues/93): CI-checked
  crate/dependency constraints with justified exceptions.
- [#94](https://github.com/Doldas/flowerly_os/issues/94): mock/real
  adapter parity and failure injection.
- [#95](https://github.com/Doldas/flowerly_os/issues/95): threat model,
  privileged authority and delegated PTY/KVM adversarial gates.

The accepted ADR-0014 Linux authority substrate, ADR-0013 process-boundary
discipline and proposed ADRs 0016/0017 all remain independently applicable.
This is a proposed architectural direction, not a runtime implementation,
a change to #50/0.0.4 acceptance or a waiver of exact-SHA Arch guest tests.
No merge without the repository owner's explicit authorization.
