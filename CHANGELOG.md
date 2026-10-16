# Changelog

All notable changes to Flowerly OS are documented here. The format follows the
spirit of Keep a Changelog, and versions use semantic-ish pre-1.0 numbering.

## Unreleased

### Architecture transition (proposed, awaiting validation)

- Unified Flowerly state handlers under `src/flowerlyd` with bounded Tokio work queues.
- Combined types, SQLite migrations and IPC framing in `flowerly-core`.
- Isolated `evolutiond` and distribution-oriented `distro/`, `tools/`, `testbed/`, `web-docs/` layouts.
- Updated CLI socket routing, systemd unit scaffolds and input validation.
- KVM execution and bootable images remain unimplemented.


### Added

- Rust Cargo workspace with the shared crates `agentic-types`,
  `agentic-storage`, `agentic-ipc`, and `agentic-core`.
- Typed, prefixed, UUID-backed identifiers and durable domain models
  (Flow, Goal, Session, Execution, Generation, ProvenanceEdge, and others).
- SQLite-backed storage with an ordered, idempotent migration runner and the
  initial schema (`migrations/0001_init.sql`).
- Unix-domain-socket, length-prefixed JSON IPC (protocol v1).
- Shared daemon foundations: `ServiceConfig`, structured tracing, and
  `run_service` with SIGTERM and IPC shutdown.
- Services `agenticd`, `flowd`, `sessiond`, `goald`, `provenanced`,
  `capabilityd`, `generationd`, and `evolutiond`.
- The `agenticctl` control-plane CLI.
- Root documentation and the architecture specification under `docs/`.

## 0.0.1 — Repository Foundation

- Initial repository structure, license, and default branch `main`.

[Unreleased]: https://github.com/Doldas/flowerly_os
