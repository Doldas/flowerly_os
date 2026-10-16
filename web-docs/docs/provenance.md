# Flowerly OS — Provenance

Provenance is the causal history of the system. Every meaningful autonomous
action should be answerable: WHO did it, WHY, WHICH goal it served, WHICH agent,
WHICH model, WHICH command, WHICH source, WHICH test and WHICH generation.
Provenance is mandatory for autonomous system changes; nothing important should
happen off the record.

## Edge model

Provenance is stored as directed edges between durable objects, defined in
`crates/agentic-types` (`ProvenanceEdge`) and owned by `provenanced`:

```text
ProvenanceEdge {
    from:       ObjectRef    // source endpoint
    relation:   ProvenanceRelation
    to:         ObjectRef    // destination endpoint
    created_at: Timestamp
    note:       Option<String>
}
```

`ObjectRef` is a strongly typed Rust enum whose variants carry the corresponding
typed ID (`FlowId`, `GoalId`, `SessionId`, and so on) or an explicit Actor
value. It serializes to a compact string for SQLite/wire inspectability and
backwards compatibility. Accepted serialized forms are:

```text
flow-...
intent-...
goal-...
agent-...
session-...
gen-...
exp-...
exec-...
obs-...
finding-...
decision-...
artifact-...
project-...
actor:<name>
```

Arbitrary untyped strings are rejected. Parsing reconstructs the enum variant,
so a valid `goal-...` reference is represented as `ObjectRef::Goal(GoalId)`
rather than losing its domain type. Domain code constructs references from typed
IDs (`ObjectRef::from(&goal_id)`) and uses the explicit `actor:` namespace for
human/system/agent actor labels. Migration 0002 normalizes legacy
`created_by` targets into that actor namespace.

## Relation vocabulary

```text
created_by   an actor created the object
caused_by    the object was caused by another object
belongs_to   the object belongs to a parent context (e.g. a goal in a Flow)
produced     an actor or run produced an artifact or result
tested_by    the object was tested by a run/experiment
promoted_to  a candidate was promoted to a generation
```

These are the canonical stored values (`created_by`, `caused_by`, `belongs_to`,
`produced`, `tested_by`, `promoted_to`).

## Service and commands

`provenanced` owns the edge store and answers queries. Duplicate edges (same
`from`, `relation`, `to`) are ignored, so recording is idempotent. The initial
operations are:

```text
provenance.add    record an edge
provenance.from   list edges originating from an object
provenance.to     list edges pointing to an object
```

Operations are reachable through `agenticctl provenance ...`. The underlying
storage lives in `crates/agentic-storage` (`insert_provenance`,
`provenance_from`, `provenance_to`).

## Why provenance exists

- **Audit:** any change can be traced to the actor, goal and evidence behind it.
- **Accountability:** agents and providers are replaceable, but their actions
  remain attributable.
- **Reversal decisions:** rollback, promotion and rejection rely on knowing what
  produced a generation and what tested it.
- **Failed experiments as knowledge:** a rejected experiment stays queryable
  rather than disappearing.

## Implementation status

- **Implemented:** the `ProvenanceEdge`/`ProvenanceRelation` model, validated
  `ObjectRef` endpoints, explicit actor references, the idempotent edge store,
  outgoing and incoming lookup through `provenance.from`/`provenance.to`,
  automatic `created_by` edges for Flows, Goals and Sessions, causal Execution
  edges to Goals/Agents, and causal Generation edges to parent generations,
  reason Goals and Experiments.
- **Planned:** multi-hop graph traversal, richer `note`/actor metadata, and
  enforcing provenance for every autonomous action as the agent runtime lands.
