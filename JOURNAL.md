# Journal

Chronological engineering notes. This is useful context for both humans and
future agents: what was built, what failed, and what comes next.

## 2026-10-11 — Repository scaffolded

Initialized the repository with `git init -b main`, configured the `origin`
remote for `flowerly_os`, and set `main` as the default branch.

## 2026-10-11 — Cargo workspace

Created the Rust workspace (resolver 2) with four shared crates and nine
binaries in total: the `agentic-*` libraries, the eight daemons, and the
`agenticctl` control plane. MSRV is Rust 1.80 and the license is
GPL-3.0-or-later.

## 2026-10-11 — `agentic-types`

Added typed, prefixed, UUID-backed identifiers (`FlowId`, `GoalId`, `AgentId`,
`SessionId`, `GenerationId`, `ExperimentId`, `ExecutionId`, and others) with
`FromStr` prefix validation and transparent serialization. Added the durable
domain models (`Flow`, `Goal`, `Session`, `Execution`, `Generation`,
`ProvenanceEdge`, `Capability`, `Artifact`) and their state enums.

## 2026-10-11 — `agentic-storage`

Implemented the SQLite-backed `Store` with an ordered, idempotent migration
runner that records applied versions in `schema_migrations`. The initial schema
(`migrations/0001_init.sql`) covers flows, projects, goals, agents, sessions,
executions, generations, observations, findings, experiments, decisions,
artifacts, provenance edges, and the user-model categories. Added CRUD and
round-trip tests.

## 2026-10-11 — `agentic-ipc`

Implemented Unix-domain-socket IPC with a 4-byte little-endian length prefix and
a UTF-8 JSON body, capped at 8 MiB. The versioned `Envelope` (protocol v1)
carries a correlation id and a tagged `Payload` (`ping`, `status`, `shutdown`,
`command`, `ok`, `err`). Added a server, a client, and a request/response test.

## 2026-10-11 — `agentic-core`

Built the shared daemon foundations: `ServiceConfig` with TOML loading and
environment overrides, `init_tracing` for text or JSON structured logs, and
`run_service`, which answers `ping`/`status`/`shutdown` and dispatches commands
to a service handler. Services exit cleanly on SIGTERM or an IPC shutdown.

## 2026-10-11 — Services and control plane

Wrote `agenticd`, `flowd`, `sessiond`, `goald`, `provenanced`, `capabilityd`,
`generationd`, and `evolutiond`. `flowd` bootstraps a default General Flow and
implements its lifecycle commands; the other services implement their initial
command sets; `capabilityd` and `evolutiond` expose status and a stub catalog.
Wrote `agenticctl` as the thin control-plane client.

## 2026-10-11 — Build and smoke test green

`cargo build`, `cargo test`, and `cargo clippy` pass. An end-to-end smoke test
starts `flowd`, queries it through `agenticctl status flowd`, creates and lists
Flows (`General` plus a new one), and shuts down cleanly.

## Upcoming — Sprint 2: Persistent State

Planned: complete migration-backed CRUD, provenance relationship queries, and
the full `agenticctl` admin surface. Not yet started.

## Upcoming — Sprint 3: Flows and Persistent Sessions

Planned: PTY-backed persistent sessions with attach, detach, and reattach, and
the Flow activation lifecycle end to end. Not yet started.

## Upcoming — Sprint 4: System Generations

Planned: generation manifests, candidate creation, activation, and rollback.
Not yet started.
