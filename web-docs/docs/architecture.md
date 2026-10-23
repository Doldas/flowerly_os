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

## Executable classification and topology regression (#105)

The two **OS supervisor daemons** are `flowerlyd` and `evolutiond`.
The workspace also includes `providerd`, an **optional, synthetic local
inference-provider plugin process**, not a third OS supervisor or a privileged
kernel boundary. Its Unix socket is a provider interface, not an inter-module
control socket. The distinction is enforced by
`python3 scripts/test-runtime-topology.py` in CI and the local docs preflight.

The topology regression rejects reintroduced legacy daemon packages and
checks that the separate Evolution boundary remains explicitly unprovisioned.
It does not replace the real two-daemon IPC black-box, privileged Arch guest,
PTY/cgroup, or release acceptance checks.

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
hypervisor explicitly reports unavailable/not-provisioned status and refuses
experiment execution with a typed `unavailable` error. It cannot execute
arbitrary host code or treat a requested provider/model profile as permission.
The specialized authorized task queue and VM lifecycle are **not implemented
yet**. Same-UID Unix IPC is retained for development only; a production
cross-account request broker must be designed and independently authorized
before `flowerlyd` can submit jobs to `evolutiond`. No unverified privileged
execution path has been added.

## ADR-0013 boundary audit — capability and Evolution stubs (#52)

**Decision:** Keep capability descriptions inside the unprivileged `flowerlyd`
process. Keep `evolutiond` as a separate, non-provisioned service boundary
for future independently authorized KVM ownership. No additional standalone
`capabilityd` process is justified. This is a *process-boundary decision*,
not approval of a production privilege broker.

| ADR-0013 criterion | Capability catalog (`flowerlyd`) | Evolution (`evolutiond`) |
| --- | --- | --- |
| Independent authority | None. Names are descriptions, never grants | Required eventually for KVM; not implemented today |
| Failure isolation | No separate security domain or privileged resource | Separate account/socket design retains prospective isolation |
| Resource ownership | No kernel authority or physical resources | Future `/dev/kvm`, VM lifecycle and isolated storage; currently unprovisioned |
| Startup/deployment | In-process module, `flowerlyd.sock` only | Separate binary and runtime directory; disabled in boot image |
| Decision | Keep in-process and explicitly non-enforcing | Keep as fail-closed, visibly unprovisioned stub |

**Current enforcement facts:** `capability.list` returns a descriptive set of
command names. It does **not** grant, check, delegate or revoke permissions,
configure seccomp, authorize host operations or authorize an agent. A named
capability must not be interpreted as a runnable command or a policy token.
The reply explicitly sets `enforced=false`, `kind=descriptive_catalog`
and `authorization=not_implemented`; legacy `capabilities` remains unchanged
for IPC v1 readers. The kernel-attested peer UID, session control tokens,
PTY guardian, cgroups and existing IPC rules remain independent of this catalog.
No new policy bypass or capability-grant API is introduced.

**Evolution World:** `evolution.experiment-status` reports `unavailable`,
`provisioned=false`, `execution_enabled=false` and
`isolation_verified=false`. The service's IPC Status details expose the same
limitations. `evolution.run_experiment` continues to reject execution with
typed `unavailable`. No KVM guest, cross-account authorization broker,
trusted delegation or isolation has been established by these fields.
Legacy status response members remain intact.

**Migration and verification:** Old clients may continue using
`capability.list` and `evolution.experiment-status`; additive fields retain
IPC v1 framing and old command names. New clients must display the explicit
unavailable/non-enforcement state rather than inferring permission from a
descriptor. The real-process integration harness checks both daemon status
and command responses, rejects a fake `capability.grant` invocation and
verifies experiment execution refusal. The issue #31 privileged-kernel
acceptance gate remains open; simulated success is not acceptable evidence.
Revisit this decision only after independently reviewed authorization,
failure-isolation, kernel/VM and account-boundary acceptance tests.

## Release-guest disk provisioning

The Arch kernel acceptance harness uses a 32 GiB sparse QCOW2 overlay by
default (`ARCH_VM_DISK_GB`, allowed 24–256). The verified upstream base image
is never resized. Cloud-init grows the root partition and filesystem; a
preflight requires at least 16 GiB total and 8 GiB free **before** `pacman`
upgrades, preserving guest capacity diagnostics in the release report.
See [building](build.md) for overrides and test coverage.

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
