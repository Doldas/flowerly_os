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

Endpoints are `ObjectRef` strings: an object's typed id, optionally tagged with
its kind (for example `flow-flow-abc123`, `goal-goal-def456`). Using strings keeps
provenance decoupled from any single object table while remaining inspectable.

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
```

Operations are reachable through `agenticctl provenance ...`. The underlying
storage lives in `crates/agentic-storage` (`insert_provenance`,
`provenance_from`).

## Why provenance exists

- **Audit:** any change can be traced to the actor, goal and evidence behind it.
- **Accountability:** agents and providers are replaceable, but their actions
  remain attributable.
- **Reversal decisions:** rollback, promotion and rejection rely on knowing what
  produced a generation and what tested it.
- **Failed experiments as knowledge:** a rejected experiment stays queryable
  rather than disappearing.

## Implementation status

- **Implemented:** the `ProvenanceEdge`/`ProvenanceRelation` model, the idempotent
  edge store, `provenanced` with `provenance.add`/`provenance.from`, and
  automatic `created_by` edges for Flows and Goals.
- **Planned:** reverse traversal and graph queries, richer `note`/actor metadata,
  and enforcing an edge for every autonomous action as the agent runtime lands.
