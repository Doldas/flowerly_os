# Changelog

All notable changes to Flowerly OS are documented here. The format follows the
spirit of Keep a Changelog, and versions use semantic-ish pre-1.0 numbering.

## Unreleased

### Distribution-first architecture transition (integration; not release-certified)

- Unified Flowerly state handlers under `src/flowerlyd` with bounded Tokio work queues.
- Combined types, SQLite migrations and IPC framing in `flowerly-core`.
- Isolated `evolutiond` and distribution-oriented `distro/`, `tools/`, `testbed/`, `web-docs/` layouts.
- Updated CLI socket routing, systemd unit scaffolds and input validation.
- KVM execution and bootable images remain unimplemented.
- Storage failures now map to IPC error codes through one shared classifier (#62). SQLite UNIQUE/PRIMARY KEY failures now return `conflict` and FOREIGN KEY failures `invalid_argument` in every service; they previously returned `internal` everywhere except Generation. Only BUSY/LOCKED returns `unavailable`.
- Flow and Goal commands use typed IPC v1 arguments: wrongly typed known fields return `invalid_argument` (for example, a non-string Goal `rationale` is no longer silently dropped), `null` means absent, and unknown fields are ignored (#53).
- `flowerlyd` Status reports `ready: true` only after the database answers one consistent count snapshot, which now includes `provenance_edge_count`. A failed read now reports `ready: false` with a typed `error_code` (#31).
- IPC servers hold an exclusive per-socket instance lock (`<socket>.lock`) and automatically recover a socket left by a crashed instance only after verifying it is stale. A second live instance is refused (#31).
- IPC deadline errors say whether the request was never executed (queued) or has an unknown outcome (handler running), with elapsed milliseconds. Every request logs a structured `ipc request` event with queue, handler and total time (#62).


### Added

- Rust Cargo workspace with the shared `flowerly-core` crate.
- Typed, prefixed, UUID-backed identifiers and durable domain models
  (Flow, Goal, Session, Execution, Generation, ProvenanceEdge, and others).
- SQLite-backed storage with an ordered, idempotent migration runner and the
  initial schema (`migrations/0001_init.sql`).
- Unix-domain-socket, length-prefixed JSON IPC (protocol v1).
- Shared daemon foundations: `ServiceConfig`, structured tracing, and
  `run_service` with SIGTERM and IPC shutdown.
- Unified `flowerlyd` for Flow, Goal, Session, Generation, Provenance,
  Capability, Compute and Execution modules; separate `evolutiond` boundary.
- The `agenticctl` control-plane CLI.
- Root documentation and architecture guides under `web-docs/docs/`.

## 0.0.1 — Repository Foundation

- Initial repository structure, license, and default branch `main`.

[Unreleased]: https://github.com/Doldas/flowerly_os
