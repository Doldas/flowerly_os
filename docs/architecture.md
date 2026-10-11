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

Domain crates (`agentic-flows`, `agentic-goals`, `agentic-sessions`,
`agentic-provenance`, `agentic-generations`, `agentic-capabilities`) exist as
directories and will join the workspace as bounded logic moves out of the
services.

## Typed identifiers and models

`agentic-types` defines prefixed, UUID-backed identifier newtypes: `FlowId`,
`GoalId`, `AgentId`, `SessionId`, `GenerationId`, `ExperimentId`,
`ExecutionId`, `ObservationId`, `FindingId`, `DecisionId`, `ArtifactId`, and
`ProjectId`. They serialize transparently and parse from strings with prefix
validation. Durable models (`Flow`, `Goal`, `Session`, `Execution`,
`Generation`, `ProvenanceEdge`, `Capability`, `Artifact`) and their state enums
live here and are shared by services and storage. The workspace forbids unsafe
code in the shared crates.

## Persistent state

Each service owns its own SQLite database under the service runtime directory
(`AGENTIC_RUNTIME_DIR`, defaulting to `$XDG_RUNTIME_DIR/flowerly`). The initial
schema is `crates/agentic-storage/migrations/0001_init.sql`, applied by an
ordered, idempotent migration runner that records applied versions in
`schema_migrations`. The schema is owned by migrations; production databases are
never mutated by hand. Tables cover flows, projects, goals, agents, sessions,
executions, generations, observations, findings, experiments, decisions,
artifacts, provenance edges, and the user-model categories (facts,
observations, inferences).

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
`AGENTIC_DATABASE_PATH`, and `AGENTIC_LOG_FORMAT` environment overrides) and
`run_service`. Services start, answer health and status without network or AI
access, and shut down cleanly on either SIGTERM or an IPC `shutdown` command.
`ServiceContext` reports name, version, pid, uptime, and protocol version.

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
