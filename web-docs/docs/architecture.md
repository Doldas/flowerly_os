# Flowerly OS architecture — distribution-first consolidation

> Architecture transition tracking: [issue #105](https://github.com/Doldas/flowerly_os/issues/105).
> This page describes the migration target; a complete release is gated on CI
> and black-box validation.

## Repository boundaries

```text
distro/       Arch rootfs inputs, image overlay and capability profiles
src/          Only trusted target Rust runtime packages
  flowerlyd/  one unprivileged supervisor, with internal modules
  evolutiond/ separately privileged Evolution World boundary
  shared/flowerly-core/  shared types, SQLite, IPC and service foundations
tools/        host-side builder and target agenticctl control-plane CLI
testbed/      KVM seeds, test fixtures and integration checks
web-docs/     MkDocs site, specs, ADRs and current documentation
```

The runtime is composed of **two daemon binaries**, not eight. Each
`flowerlyd` subdomain retains a separately testable Rust module:
`agentic` (execution), `capability`, `flow`, `generation`, `goal`,
`provenance`, and `session`. The old names remain accepted as CLI service
aliases, but map to **one** `flowerlyd.sock`. Only `evolutiond` owns a
separate privileged control-plane socket.

## In-process coordination

The `flow`, `generation` and `provenance` handlers receive commands via
bounded `tokio::sync::mpsc` queues. Each task carries a correlated reply,
limits queue depth and preserves the existing handler deadline. Other modules
are invoked directly in-process. No subdaemon Unix sockets are created.
External requests remain protocol-v1, length-prefixed JSON over Unix sockets,
with kernel-attested peer UID rather than trusting client-created identities.

## State and PTYs

The SQLite `state.db`, ordered migration files and domain models are kept in
one `flowerly-core` library. On startup `flowerlyd` migrates the state once,
ensures the General Flow, and initializes the existing Linux PTY supervisor
with stale-cgroup reconciliation. The PTY guardian remains an independently
supervised **child process** for containment; this is not a logical Flowerly
microservice or an extra database daemon.

## Privilege boundary

`evolutiond` must run as a different system account, with explicit access to
`/dev/kvm` and a **separate** private runtime and state directory. The current
hypervisor only advertises not-provisioned/idle status. It cannot execute
arbitrary host code or treat a requested provider/model profile as permission.
The specialized authorized task queue and VM lifecycle are **not implemented
yet**. No unverified privileged execution path has been added.

## Distribution artifacts and limitations

`distro/base-image/arch` holds mkosi/Arch package definitions.
`distro/overlay` supplies two hardened systemd service units.
`tools/distro-builder validate` checks manifests; `build` explicitly fails
because bootable image generation is not implemented. `testbed/` captures
integration and KVM acceptance seeds.

## Validation gates

Compilation, Rust unit tests, IPC/authentication contract coverage, durable
operation-id replay, SQLite contention, PTY guardian/failure injection and
Arch/Linux process tests must be considered before merging. Green unit tests
alone are insufficient evidence for system process safety.
