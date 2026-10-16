# 0017: Unified and Mode-Driven Storage Subsystem

Date: 2026-10-14  
Status: Proposed — architecture review required  
Proposes to supersede: [ADR-0011](0011-shared-local-state-database.md) physical topology after verified migration  
Retains: [ADR-0005](0005-sqlite-as-initial-state-store.md) SQLite, transactional constraints and versioned migrations

## Context

Flowerly OS needs a locally recoverable operating-system foundation, persistent
asynchronous agent memory with optional semantic vector search, and optional
multi-machine sharing of bounded, verified intelligence and threat indicators.
These are distinct availability, consistency, confidentiality and authority
domains. A developer operating the system alone should not need to maintain
PostgreSQL, MongoDB or Redis for the OS core, open a database network port on a
standalone device, or depend on an AI provider to boot.

Today the accepted [ADR-0011](0011-shared-local-state-database.md) and the
implemented `agentic-storage` use **one shared local SQLite `state.db`**.
Flow, Goal, Session, Generation and provenance relationships currently depend
on its cross-domain transactional and foreign-key properties. Separating
physical databases without auditing these invariants is unsafe. This ADR
therefore defines **proposed target architecture**, not deployed behavior;
the accepted physical topology stays in force until a migration is proven,
reviewed and explicitly approved.

## Proposed decision

Use a trait-driven storage subsystem in `crates/agentic-storage` with two
logical domains, each independently configurable and separately trusted.

### Local system core: `system_state.db`

Standard embedded SQLite, always local, never replicated. Owns boot-critical
and recovery-critical structured configuration, local UID/identity mappings,
Generation/activation/provenance data and references required to reconstruct
local filesystem generations. Preserve stable IDs, schema constraints, WAL,
foreign keys, migration ownership and an explicit backup/restore path.
Application-level immutability alone is insufficient: enforce authorized
mutations, least-privilege filesystem ownership, recovery integrity checks and
generation rollback semantics. The objective is recoverability, **not an
unconditional guarantee that disk state cannot be destroyed**.

### Agent memory: `agentic_memory.db`

Stores authorized agents' long-term memory, permitted conversation history,
script metadata and model-versioned vector embeddings. Standalone stores it as
local SQLite with an evaluated in-process semantic search implementation.
Cluster may select a libSQL/Turso-compatible replicated backend **only after
its actual supported replication, concurrency and conflict semantics are
measured and approved**. No memory availability or network dependence may
block core boot. Memory storage must enforce per-actor/tenant access controls,
data minimization, retention, redaction and deletion independently of
retrieval ranking.

No cross-database foreign key or ACID guarantee is implied by the split.
Core invariants formerly covered by a single SQLite transaction must remain
local to one core transaction, be redesigned using explicit durable protocols,
or block migration until resolved. Never silently replace transactional
atomicity with eventual consistency for provenance or activation.

## Storage modes

| Mode | System core | Agent memory | Network requirements |
| --- | --- | --- | --- |
| `Standalone` (default) | Local SQLite | Local SQLite + in-process vector index | None |
| `Cluster` | Local SQLite, never replicated | Optional verified libSQL/Turso mesh | Authenticated, encrypted opt-in links only |
| `CoreOnly` | Minimal sufficient local SQLite | Not opened by core boot | None |

The eventual installation configuration may live at
`/etc/flowerly/mode`. Its schema, ownership, upgrade policy and
trust boundaries are deliverables; do not treat that path as implemented now.
Missing or malformed configuration must default safely according to an
explicitly tested installation policy; selecting `Cluster` may never
happen as an implicit recovery shortcut.

In `CoreOnly`, stop/pause cognitive services such as `goald` and
`agenticd` through explicit dependencies and lifecycle rules while
preserving OS recovery and necessary local-state operations. CoreOnly
must never delete memory merely because it is disabled.

## Rust interface and feature boundaries

Domain-oriented traits should separate system-state repositories,
memory repositories, vector indexes and replication controllers, with
strongly typed objects and errors. Favor focused traits and query models
over a universal method exposing arbitrary SQL to cognitive services.
An illustrative shape, subject to compatibility and transactional review:

```rust
pub enum StorageMode {
    Standalone,
    Cluster,
    CoreOnly,
}

pub struct VectorEmbedding {
    pub id: String,
    pub model_id: String,
    pub dimensions: usize,
    pub values: Vec<f32>,
}

pub trait SystemStateRepository: Send + Sync {}

pub trait MemoryRepository: Send + Sync {}

pub trait EmbeddingIndex: Send + Sync {}

pub trait ReplicationController: Send + Sync {}
```

Feature-gate optional memory backends, keeping native bundled SQLite as
the installed default. Proposed Cargo feature names are
`sqlite-native` and `turso-mesh`. Do **not** pin example versions
(`rusqlite = 0.31`, `libsql = 0.6`) into the live workspace without
an up-to-date compatibility/security audit: the workspace currently
uses `rusqlite = 0.40`, a unified Rust toolchain policy, and existing
transactional APIs. Verify actual upstream library capabilities instead
of promising generic multi-writer MVCC without tests.

Rust static typing reduces invalid application states, but does **not**
prove the correctness of arbitrary SQL, dynamically generated schemas,
migration operations or untrusted agent input. Validate at API,
authorization, database-constraint and execution-boundary levels.

## Cluster security and threat data

mDNS may discover peers but must **never authenticate them**.
Cluster enrollment requires explicit operator approval, independently
verified identities, authenticated encryption, key rotation, revocation
and per-peer authorization. Do not replicate core/boot tables,
credentials or private conversations without an explicit data-sharing
policy. Threat indicators are provenance-tagged inputs subject to
validation, signatures, expiry and quarantine. Receiving a threat
indicator must not automatically authorize Stable World modification
or remote execution; any response must pass local capability and
generation-promotion gates.

An offline node must preserve its last verified local core state.
Partition repair, merge/conflict policy, replay resistance, sync cursors,
backpressure and schema/version compatibility are required before
Cluster acceptance.

## Migration, compatibility and operations

Before physically splitting `state.db`, inventory and preserve all
current cross-domain transactions, ID links, provenance constraints and
the `AGENTIC_DATABASE_PATH` recovery override. Provide idempotent
versioned migration, backup, staging, rollback and restart-at-any-point
tests. Prove CoreOnly boot with missing/corrupt memory while maintaining
safe, locally recoverable core state. Keep migrations explicit and
reviewable; no agent may invent a database schema in Stable World.

Mocks make portable testing easier, but real SQLite migrations,
filesystem durability and real mesh behavior still need end-to-end
Linux/Arch/KVM tests. No separate database server is necessary for
standalone core use; installed dependencies and actual resource use
must be measured rather than assumed.

## Delivery plan

1. [#82](https://github.com/Doldas/flowerly_os/issues/82): inventory and approve target topology/invariants.
2. [#83](https://github.com/Doldas/flowerly_os/issues/83): typed repository/backend interfaces and mocks.
3. [#84](https://github.com/Doldas/flowerly_os/issues/84): versioned, recoverable dual-database migration.
4. [#85](https://github.com/Doldas/flowerly_os/issues/85): mode schema, safe defaults and transitions.
5. [#86](https://github.com/Doldas/flowerly_os/issues/86): standalone local semantic memory.
6. [#87](https://github.com/Doldas/flowerly_os/issues/87): optional libSQL/Turso mesh research and hostile-peer acceptance.
7. [#88](https://github.com/Doldas/flowerly_os/issues/88): CoreOnly service lifecycle and provider-free boot.
8. [#89](https://github.com/Doldas/flowerly_os/issues/89): cross-mode operations and exact-SHA Arch/KVM evidence.

## Status and acceptance

This ADR is **proposed**. ADR-0011 remains accepted until explicit
supersession. These tasks do not change existing 0.0.4 PTY security
release gates, close issue #50, or trigger unrelated issue #55.
Do not merge implementation without owner's explicit authorization.
