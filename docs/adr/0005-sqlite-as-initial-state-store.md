# 0005: SQLite as the Initial Durable State Store

Date: 2026-10-11
Status: Superseded in part by ADR-0011

## Context

The first services need durable, inspectable, low-operations structured state
for Flows, Goals, Sessions, Executions, Generations and provenance edges. A
client/server database would add operational complexity that a single-machine,
local-first system does not need yet. The schema will change frequently during
bootstrap, so controlled schema evolution is essential.

## Decision

SQLite is the initial durable structured state store. Schema is owned by an
ordered, idempotent migration mechanism from day one. Migrations live in
`crates/agentic-storage/migrations/` and are applied by `agentic-storage`; the
production schema is never mutated by hand outside a migration. `rusqlite` is
used with the `bundled` feature so the correct SQLite version is compiled into
the binary, and foreign keys plus WAL are enabled by the storage layer.

The original version of this ADR also selected one physical database file per
service. That topology is superseded by
[ADR-0011](0011-shared-local-state-database.md), which keeps SQLite and the same
migration discipline but uses one local `state.db` by default.

## Consequences

- Zero-operations, single-file, easy-to-back-up durable state with ACID semantics.
- Migrations are versioned, testable and idempotent; applying them twice is safe.
- Physical database topology is no longer defined here; see ADR-0011.
- Missing records and corruption are surfaced as errors, never silently ignored.
- Moving to another store later must preserve the migration history and IDs.
- Implemented today: `crates/agentic-storage`, with migrations and CRUD.
