# Flowerly OS — Flow Model

A Flow is a persistent, evolvable operating environment representing one
coherent human activity context: General, Development, Server Operations, Art,
Gaming, Research, or a project-specific environment. Flows are first-class
objects, not windows, so users switch contexts instead of reconstructing them.
See [`../adr/0008-flows-as-first-class-os-objects.md`](../adr/0008-flows-as-first-class-os-objects.md).

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

## States

The implemented `FlowState` enum has five values: `FOREGROUND`, `ACTIVE`,
`QUIESCENT`, `HIBERNATED`, `ARCHIVED`. The bootstrap milestone fully supports
`FOREGROUND`, `ACTIVE` and `ARCHIVED`; `QUIESCENT` and `HIBERNATED` exist in the
model so the data model does not block later lifecycle work.

```text
FOREGROUND  user is interacting with the Flow now
ACTIVE      running normally in the background (initial state of a new Flow)
QUIESCENT   interactive state kept, expensive activity reduced (planned)
HIBERNATED  runtime state frozen or reconstructable (planned)
ARCHIVED    retained for memory and provenance, not running
```

## The General Flow

Every installation always has a `General` Flow: low-configuration, neutral, the
default landing context and a safe target for temporary work. `flowd` ensures it
exists on startup by checking for a Flow named `General` and creating one owned
by `system` if absent.

## Activation and archiving

Activation makes a Flow the current context and updates `last_active`; the
`FOREGROUND` state is the presentation of the active Flow. Archiving moves a
Flow to `ARCHIVED` without deleting its record, so its history and provenance
remain. Operations are exposed through `agenticctl flow create|list|activate|archive`.

## Provenance

Creating or promoting a Flow records a provenance edge whose relation is
`created_by` (relation `CreatedBy`), with the creating actor recorded in both the
`created_by` field and the edge. See [`PROVENANCE.md`](PROVENANCE.md).

## Implemented status and planned extensions

- **Implemented:** the `Flow`/`FlowState` model, the General Flow, persistence
  across restarts, create/list/activate/archive, and `created_by` provenance.
- **Planned:** `QUIESCENT`/`HIBERNATED` transitions, nested Flow ergonomics,
  Flow revisions, per-Flow resource policy, cgroup/namespace isolation,
  Flow-aware credentials, notifications and templates.
