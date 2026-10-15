# Architecture

This document describes the architecture that exists in the repository today,
followed by the target architecture that the current primitives are growing
toward. Everything in the implemented sections below is real code; everything
after "Target architecture" is planned unless explicitly stated.

## Implemented: Rust workspace

A single Cargo workspace (resolver 2, MSRV Rust 1.80):

```text
crates/agentic-types      typed IDs and durable domain models
crates/agentic-storage    SQLite store and migration runner
crates/agentic-ipc        Unix-socket, length-prefixed JSON IPC (protocol v1)
crates/agentic-core       ServiceConfig, run_service, structured tracing
services/agenticd         execution records
services/flowd            Flow lifecycle and the default General Flow
services/sessiond         session metadata
services/goald            persistent goals
services/provenanced      provenance edges
services/capabilityd      capability catalog
services/generationd      generation manifests
services/evolutiond       experiment status (stub)
services/agenticctl       human/agent control plane CLI
```

Additional domain crates such as `agentic-flows`, `agentic-goals`,
`agentic-sessions`, `agentic-provenance`, `agentic-generations`, and
`agentic-capabilities` are **planned, not currently present**. They should only
be introduced when bounded reusable logic actually needs to move out of a
service; conceptual domains do not justify empty crates by themselves.

## Typed identifiers and models

`agentic-types` defines prefixed, UUID-backed identifier newtypes: `FlowId`,
`IntentId`, `GoalId`, `AgentId`, `SessionId`, `GenerationId`, `ExperimentId`,
`ExecutionId`, `ObservationId`, `FindingId`, `DecisionId`, `ArtifactId`, and
`ProjectId`. They serialize transparently and parse from strings with prefix
validation. Durable models (`Flow`, `Goal`, `Session`, `Execution`,
`Generation`, `ProvenanceEdge`, `Capability`, `Artifact`) and their state enums
live here and are shared by services and storage. The workspace forbids unsafe
code in the shared crates.

## Linux distribution substrate

Flowerly OS uses Arch Linux as its initial userspace/package substrate while
retaining the upstream Linux kernel policy from ADR-0004. Arch packages are
ordinary build inputs; Flowerly owns generation construction, testing,
promotion and rollback.

```text
Arch repositories ──→ candidate generation ──→ Evolution World
                                              │
                                  promote ─────┘
                                      ↓
                                 Stable World
```

Stable World is therefore not intended to track Arch repositories by unmanaged
system-wide upgrades. The first declarative image inputs live under
`image/arch/`; bootable image output is still planned. See
[image-model.md](image-model.md) and
[ADR-0015](adr/0015-arch-linux-userspace-substrate.md).

## Persistent state

Local services share one physical SQLite database, `state.db`, by default.
`AGENTIC_RUNTIME_DIR` is reserved for ephemeral sockets/runtime files while
`AGENTIC_STATE_DIR` holds durable state. `AGENTIC_DATABASE_PATH` remains an
explicit test/recovery override.

Services still own their domain APIs and tables logically; sharing the physical
database does not authorize arbitrary cross-domain access. The shared store
keeps local causal relationships transactional and makes cross-domain foreign
keys meaningful without inventing distributed transactions.

The initial schema is `crates/agentic-storage/migrations/0001_init.sql`, with
subsequent changes in numbered migrations. Migrations run inside an IMMEDIATE
transaction and re-check their version after acquiring the writer lock, making
concurrent daemon startup safe. The schema is owned by migrations; production
databases are never mutated by hand. See
[ADR-0011](adr/0011-shared-local-state-database.md).

## IPC

Services communicate over Unix domain sockets using a deliberately simple
protocol: a 4-byte little-endian length prefix followed by a UTF-8 JSON body
(maximum 8 MiB). An `Envelope` carries a protocol version (`1`), a correlation
id, and a tagged `Payload` (`ping`, `status`, `shutdown`, `command`, `ok`,
`err`). `agentic-core` provides `run_service`, which answers `ping`, `status`,
and `shutdown` centrally and dispatches `command` payloads to the service
handler. The wire format stays inspectable with ordinary tools.

## Daemon lifecycle

Every daemon uses `ServiceConfig` (a TOML file plus `AGENTIC_RUNTIME_DIR`,
`AGENTIC_STATE_DIR`, `AGENTIC_DATABASE_PATH`, and `AGENTIC_LOG_FORMAT`
environment overrides) and `run_service`. Services start, answer health and
status without network or AI access, and shut down cleanly on either SIGTERM or
an IPC `shutdown` command. `ServiceContext` reports name, version, pid,
uptime, and protocol version.

Conceptual domains do not automatically imply process boundaries. A future
component becomes a daemon only when privilege separation, fault isolation,
independent lifecycle, trust boundaries, resource accounting, or materially
different runtime needs justify IPC. Otherwise prefer a crate/module behind an
explicit interface. See
[ADR-0013](adr/0013-process-boundaries-require-justification.md).

## Observability

Structured logging uses `tracing` and `tracing-subscriber`, written to stderr so
journald captures it under systemd. Output is human-readable text by default and
JSON lines when configured. Log records carry identity fields such as `flow_id`
and `goal_id` rather than encoding identity only in free text.

## Control plane

`agenticctl` is a thin client over the same IPC protocol the future GUI shell
will use. It exposes `status` and `ping` plus `flow`, `goal`, `session`,
`generation`, `provenance`, `execution`, and `capability` subcommands.

## Testing

Unit tests cover IDs, state transitions, storage round-trips, migrations,
configuration, and the IPC request/response path. An integration test starts the
`flowd` binary, waits for its socket, queries status, creates a Flow, and
verifies clean shutdown.

## Proposed Layer 1.5 facade boundary

[ADR-0018](adr/0018-strict-boundary-isolation-layer.md) proposes
isolating external storage drivers and Linux kernel operations behind
typed, auditable adapters, with Layers 2–5 of the control plane using
well-defined Rust interfaces. This is **not implemented**; existing
`agentic-storage` still directly uses `rusqlite`, and services retain
current trusted OS/PTY operations. Any migration must preserve the
single-database ACID model in accepted ADR-0011 until approved
replacement, and maintain real kernel-credential/cgroup enforcement.
Tracked through #91–#95 and the storage work #82–#89.

For proposed implementation contracts, physical migration/recovery controls,
Linux trusted-boundary exceptions, evidence criteria and ordered work, see the
[architecture design packet](design/README.md). These are plans only.

## Target architecture (planned)

The current services are the durable skeleton for a larger design. The target
conceptual model separates a **Stable World** (the user's actual system, at
generation N) from an **Evolution World** (QEMU/KVM candidates, experiments,
destructive testing). **Flows**, **Sessions**, **Goals**, and **Generations**
become fully persistent first-class objects, and **provenance** forms a
queryable causal graph across observations, goals, executions, experiments,
commits, builds, and generations. Service supervision moves to ordinary systemd
units. None of this replaces Linux: structured Agentic APIs are abstractions
over existing kernel and userspace primitives, not replacements for them.
