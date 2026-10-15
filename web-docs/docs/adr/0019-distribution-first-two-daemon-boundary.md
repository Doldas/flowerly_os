# ADR-0019: Distribution-first two-daemon architecture

- Status: **Proposed / under implementation**
- Issue: [#105](https://github.com/Doldas/flowerly_os/issues/105)

## Decision

Consolidate former local state daemons into `flowerlyd`; keep `evolutiond`
as a separate Unix identity and KVM privilege domain. Share types, SQLite
storage, IPC framing and daemon foundations in `flowerly-core`. Separate host
distribution tooling, target runtime, distro inputs, testbed and MkDocs sources.

## Rationale

The legacy eight-daemon topology paid process/socket overhead for services
sharing one SQLite file and one trust boundary. Bounded Tokio in-memory
queues preserve modularity without redundant network-facing interfaces.

## Non-goals

This refactor does not make KVM provisioning, a bootable ISO, remote
cross-device sessions, unrestricted agent privileges, provider selection or
enterprise coordination available. No privilege escalation is permitted.

## Evidence required

Cargo checks, migration compatibility, black-box CLI/API probes, PTY
cgroup and crash handling, and Arch guest acceptance on exact release SHA.
