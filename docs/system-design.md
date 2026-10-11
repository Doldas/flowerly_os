# Flowerly OS — Architecture

This document describes the technical architecture of Flowerly OS: what exists
today and what is planned. For long-term direction see the grounding
specification `AGENTIC_OS_REPOSITORY_ARCHITECTURE_SPRINTS_AND_OPENCODE.md` in
this directory, and the architecture decision records in `adr/`.

Status labels used below: **Implemented** (present and tested), **In progress**
(partially present), **Planned** (designed, not built), **Research** (exploratory).

## Stable World vs Evolution World

Flowerly OS separates two execution worlds on the same physical machine:

```text
                      PHYSICAL MACHINE
   ┌───────────────────────────────────────────────┐
   │                STABLE WORLD                    │
   │  generation N — the user's real environment    │
   ├───────────────────────────────────────────────┤
   │               EVOLUTION WORLD                  │
   │  QEMU/KVM — candidate N+1, experiments,        │
   │  destructive testing                           │
   └───────────────────────────────────────────────┘
```

The Stable World is what the user depends on; it changes only through promoted,
reversible generations. The Evolution World is an isolated KVM sandbox where
aggressive, potentially destructive experiments run without risk to the Stable
World. Evidence from the Evolution World drives a promote/reject decision. See
[evolution.md](evolution.md) and [generations.md](generations.md).

## Agentic runtime daemons

The service boundaries below stay clear even when an implementation combines responsibilities.

| Daemon | Responsibility | Status |
| --- | --- | --- |
| `agenticd` | orchestration, agent lifecycle, model invocation, execution records | Implemented; bounded agent runtime planned |
| `flowd` | Flow lifecycle, General Flow, activation/archive | Implemented |
| `sessiond` | durable session identity and state | Implemented; PTY plumbing planned |
| `goald` | persistent goals and goal graph | Implemented |
| `generationd` | generation manifests, status, activation, rollback | Implemented metadata; real snapshots planned |
| `provenanced` | directed causal edges between durable objects | Implemented |
| `capabilityd` | capability catalog, identity, policy | Catalog implemented; policy planned |
| `evolutiond` | experiments, KVM orchestration, promotion | Stub; Evolution World planned |

Each daemon is a foreground process with a uniform status/shutdown contract.

## Typed IDs (see `crates/agentic-types`)

Durable objects share strongly typed, prefixed, UUID-backed identifiers from
`crates/agentic-types`, so logs, provenance edges and the database all refer to
the same stable value instead of raw strings:

```text
flow-…  goal-…  agent-…  session-…  gen-…  exp-…  exec-…  obs-…
finding-…  decision-…  artifact-…  project-…
```

`ObjectRef` strings (for example `flow-flow-abc123`) are the endpoints of
provenance edges.

## Persistent storage

Each service owns a SQLite database. Schema is owned by an ordered, idempotent
migration mechanism in `crates/agentic-storage/migrations/`; the production
schema is never mutated by hand. `rusqlite` is built with the `bundled` feature,
and foreign keys and WAL are enabled by the storage layer. See
[ADR-0005](adr/0005-sqlite-as-initial-state-store.md).

## IPC: Unix sockets, Protocol v1

Services communicate over Unix domain sockets using a small versionable JSON
protocol, implemented in `crates/agentic-ipc`:

```text
Envelope { version, id, payload }      // Payload is flattened and tagged by "type"
Payload  = ping | status | shutdown | command{name,args} | ok{data} | err{message}
```

Framing is a little-endian `u32` length prefix followed by UTF-8 JSON, which
keeps the wire format inspectable and easy to evolve. `PROTOCOL_VERSION` is `1`.
Frames above `MAX_FRAME_BYTES` (8 MiB) are rejected. The protocol is local-only;
no distributed RPC platform is introduced at this stage.

## Observability

Services emit structured logs through `tracing`/`tracing-subscriber`, in text or
JSON form, to stderr where journald captures them unmodified. Important records
carry typed IDs (`flow_id`, `goal_id`, `session_id`, `generation_id`, …) rather
than encoding identity only in free-form messages. See
`crates/agentic-core/src/logging.rs`.

## Control plane: agenticctl

Humans and agents use the same command-line control plane, `agenticctl`, a thin
client over the local IPC protocol. It currently supports `status`, `flow`,
`goal`, `session`, `generation`, `provenance`, `execution` and `capability`
operations. No privileged logic lives in the client.

## Packaging and image

- systemd unit files live in `packaging/systemd/` — **Planned** (see
  [ADR-0006](adr/0006-systemd-service-supervision.md)).
- Reproducible root filesystem, initramfs and bootable image work is **Planned**
  (`image/`).
- Kernel configuration is **Planned** in `kernel/config` (see
  [ADR-0004](adr/0004-upstream-linux-kernel-policy.md)).

## Workspace layout

```text
crates/     shared Rust infrastructure (types, storage, ipc, core) and domain crates
services/   system daemons
tools/      agenticctl control plane
docs/       documentation, decisions, proposals
  adr/      architecture decision records
  rfcs/     design proposals
  specs/    grounding specifications
scripts/    development tooling
```
