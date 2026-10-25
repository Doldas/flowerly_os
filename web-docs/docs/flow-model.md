# Flowerly OS — Flow Model

A Flow is a persistent, evolvable operating environment representing one
coherent human activity context: General, Development, Server Operations, Art,
Gaming, Research, or a project-specific environment. Flows are first-class
objects, not windows, so users switch contexts instead of reconstructing them.
See [ADR-0008](adr/0008-flows-as-first-class-os-objects.md).

## Data model

The model is implemented in `crates/agentic-types` (`Flow`) and persisted by
`flowd` through `crates/agentic-storage`:

```text
Flow {
    id:          FlowId        // flow-<uuid>, stable for the Flow's lifetime
    name:        String        // "General", "Development", ...
    purpose:     Option<String>
    parent_flow: Option<FlowId>  // supports nested contexts
    state:       FlowState
    created_by:  String        // "user", "agent", "system"
    created_at:  Timestamp
    last_active: Option<Timestamp>
    revision:    u32           // revision of the Flow's own configuration
}
```

## Separation of concerns

The bootstrap `Flow` struct is intentionally the stable identity/root aggregate,
not the final home for every future Flow concern.

The planned decomposition is:

```text
Flow
├── identity / purpose / hierarchy
├── FlowConfig revision
├── FlowRuntime state
├── FlowPresentation state
└── FlowPolicy references
```

This prevents resource state, adaptive UI layout, credentials and durable Flow
identity from collapsing into one giant record. Exact structs/tables are not
created until implementation evidence requires them. See
[RFC 0002](rfcs/0002-flow-state-decomposition.md).

## States

The implemented `FlowState` enum is the Flow's **lifecycle** and has four
values: `ACTIVE`, `QUIESCENT`, `HIBERNATED`, `ARCHIVED`. The bootstrap milestone
fully supports `ACTIVE` and `ARCHIVED`; `QUIESCENT` and `HIBERNATED` exist in the
model so the data model does not block later lifecycle work. Which Flow a user
is working in is **not** a lifecycle state; see
[the per-user foreground](#activation-the-per-user-foreground-and-archiving).

```text
ACTIVE      running normally (initial state of a new Flow)
QUIESCENT   interactive state kept, expensive activity reduced (planned)
HIBERNATED  runtime state frozen or reconstructable (planned)
ARCHIVED    retained for memory and provenance, not running
```

## The General Flow

Every installation always has exactly one `General` Flow: low-configuration,
neutral, the default landing context and a safe target for temporary work.
`flowerlyd` ensures it exists on startup and that it is the shared `system`
Flow. Every admitted user can see it, select it and attach Sessions to it;
nobody can archive, rename or delete it.

## Activation: the per-user foreground, and archiving

Each Linux user (kernel-attested uid) has their **own** foreground Flow (#225,
migration 0010). It is stored in `flow_foreground`, with at most one row per
uid (its primary key). A uid with no row is in General.

- `flow.activate` on one of your own Flows records it as your foreground and
  updates its `last_active`. Activating General deletes only your own row.
  Neither ever changes another user's foreground, and General's shared row
  is never modified by anyone's choice.
- Only an unarchived Flow owned by the same uid can be stored. The database
  enforces this for every writer, using triggers on `flow_foreground`.
  General, other system Flows, unresolved Flows and other users' Flows can
  never be stored. Archiving a Flow, transferring its owner or deleting it
  removes it as anyone's foreground in the same statement, so that user
  falls back to General.
- `flow.list` shows your own Flows plus General. Each item carries the
  lifecycle `state` and, separately, `foreground: true|false` for *you*. Status
  reports your `foreground_flow`. Another user's Flow is answered exactly like
  a missing one (`not_found`).
- Archiving moves one of your own Flows to `ARCHIVED` without deleting its
  record, so its history and provenance remain. Archiving General is
  `permission_denied`. Activating an archived Flow is `conflict`.

Operations are exposed through `agenticctl flow create|list|activate|archive`.

**Upgrade (migration 0010, one transaction).** The single shared foreground of
earlier releases goes to its owner, if that owner is a resolved user. A
General, system or unresolved foreground becomes the General fallback for
everyone. All former `FOREGROUND` rows become `ACTIVE`, and the old
single-foreground index is dropped in the same transaction. A failure leaves
the database at schema 9, unchanged.

## Provenance

Creating or promoting a Flow records a provenance edge whose relation is
`created_by` (relation `CreatedBy`), with the creating actor recorded in both the
`created_by` field and the edge. See [provenance.md](provenance.md).

## Implemented status and planned extensions

- **Implemented:** the `Flow`/`FlowState` model, the General Flow, persistence
  across restarts, create/list/activate/archive, and `created_by` provenance.
- **Planned:** `QUIESCENT`/`HIBERNATED` transitions, nested Flow ergonomics,
  Flow revisions, the Flow config/runtime/presentation/policy decomposition,
  per-Flow resource policy, cgroup/namespace isolation, Flow-aware credentials,
  notifications and templates.
