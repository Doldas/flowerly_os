# #83 — inactive typed optional-memory contract

Status 2026-11-07: **implemented only as Rust library types and tests**, not as a production memory backend. The existing accepted single-SQLite core remains authoritative.

New `storage::memory_contract` defines owner-scoped typed memory CRUD, versioned model/dimension embeddings, and a separate memory-only replication status contract. A validated vector rejects non-finite values, zero dimension and impossible sizes; unavailable backends must return an error rather than fake success. Interfaces never accept arbitrary SQL, DB handles, PIDs, or filesystem paths.

Remaining: real SQLite adapter, runtime wiring, authenticated owner provenance, command transaction parity, mocks that exercise real adapter semantics, memory indexes and full release acceptance. No physical database split; ADR-0017 remains Proposed. No merge without explicit owner approval.
