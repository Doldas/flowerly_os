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
- `provider.register` refuses a non-string `kind` (it was silently registered as `local-unix`) and non-string `capabilities` entries (they were silently dropped) (#53).
- PTY writer-lease rules (exclusivity, 60 s expiry, renewal, revoked and forged tokens) are factored into pure functions with deterministic expiry tests; the same-UID isolation limits are documented (#73).
- **Security (#225):** the shipped `flowerlyd` unit is temporarily service-account-only. It no longer sets `AGENTIC_USERS_GROUP`; `Group=flowerly` and `RuntimeDirectoryMode=0700` keep the socket private, because `flowerly-users` members could read and change each other's data. Admin group membership alone no longer admits a different-UID peer. Admin commands stay denied by default; a root-only out-of-band procedure is documented. A new offline check rejects any shipped configuration that re-enables admission.
- Compute idempotency and silent-default fixes from the #53 audit. A retried `profile.clone` or `provider.register` with the same `operation_id` now returns the stored object instead of a never-created id, and a reused key with other arguments is `conflict`. `profile.edit` honors `operation_id` instead of appending a duplicate version. `usage.record` refuses `operation_id` instead of ignoring it, and `profile.rename` validates it. `workload.list` accepts `"mine": null`. `inference.request` no longer assumes the `ask` fallback for an unresolvable profile, or records a missing provider latency as zero.
- `flowerlyd` enforces administrative authorization by Linux group (#31). Admin commands and IPC `shutdown` require kernel-verified membership of `AGENTIC_ADMIN_GROUP` (`SO_PEERCRED` plus `SO_PEERGROUPS`); with no admin group they are refused for everyone, including the service account. `AGENTIC_USERS_GROUP` members may connect (socket mode 0660) and run User commands. PTY commands are limited to the service account. An unknown group stops startup. The systemd unit uses `flowerly-users` and `flowerly-admin`. **Upgrade:** add administrators to `flowerly-admin`, or Admin commands stay refused.
- Every `flowerlyd` command is classified User or Admin in one inventory, and the router dispatches only classified commands. Status reports the transitional `admin_authorization` policy (#31).
- PTY caller mistakes (cursor past the end or negative, malformed `wait_ms`, write data or dimensions) now return `invalid_argument` instead of retryable `unavailable`. The long-poll contract is covered by a new adversarial test (#73).
- Session metadata commands use typed IPC v1 arguments: a non-string `kind` no longer silently becomes `local-pty`, and a non-string `cwd` is no longer dropped (#53).
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
