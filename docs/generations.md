# Flowerly OS — Generations

A generation is a reproducible, reversible snapshot of system state. System
changes move through candidate generations, are tested in the Evolution World,
and are promoted or rejected based on evidence. User data is kept separate from
system-generation rollback: rolling back the system never discards user data.

## Data model

Defined in `crates/agentic-types` (`Generation`) and persisted by
`generationd` through `crates/agentic-storage`:

```text
Generation {
    id:              GenerationId // gen-<uuid>
    parent:          Option<GenerationId>
    source_commit:   Option<String>  // git commit the candidate was built from
    system_manifest: Option<String>  // hash of the system manifest
    reason_goal:     Option<GoalId>  // the goal that motivated the change
    experiment:      Option<ExperimentId>
    status:          GenerationStatus
    created_at:      Timestamp
}
```

## Status lifecycle

The intended lifecycle is `CANDIDATE`, `STABLE`, `SUPERSEDED` and `REVOKED`:

- `CANDIDATE` — built or being built and not yet trusted.
- `STABLE` — verified and currently active on the machine.
- `SUPERSEDED` — a newer stable generation replaced it.
- `REVOKED` — withdrawn because a defect or risk was discovered.

> Implementation note: the current `crates/agentic-types::GenerationStatus` enum
> uses `CANDIDATE`, `TESTED`, `STABLE`, `REJECTED` and `SUPERSEDED`. `TESTED`
> records a candidate that booted and passed tests in the Evolution World;
> `REJECTED` records an explicit evaluate-and-reject decision. The vocabulary
> above is the intended model; reconciling `TESTED`/`REJECTED`/`REVOKED` is
> planned.

## Activation and rollback

Activating a generation supersedes the previously active stable generation. On
activation, the prior stable generation's status becomes `SUPERSEDED` and the
activated one becomes `STABLE`; rollback reactivates a known prior generation.
Generation history survives reboot. Operations are exposed through
`generationd` and `agenticctl generation list|create|inspect|activate|rollback`.

## Implemented status and planned extensions

- **Implemented:** the `Generation` model, status transitions, persistence and
  provenance links to source commits, goals and experiments. `generationd` records
  **metadata only** at this stage.
- **Planned:** real root-filesystem snapshots, a true system manifest, actual
  activation/rollback against the installed system, and coordinated boot loader
  integration. The initial generation system may manage a constrained test root
  filesystem before the full installed distribution.
