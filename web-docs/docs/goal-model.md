# Flowerly OS — Goal Model

Goals are first-class operating-system objects owned by `goald`. A Goal is a
persistent statement of work with a rationale, constraints, success criteria and
a priority. Goals are what agents and the evolution loop act on, and they are
the anchor that provenance edges point back to.

A Goal is deliberately distinct from an **Intent**. An Intent can resolve to an
immediate contextual action ("show the servers") without creating durable work,
while an Intent such as "reduce boot time by 20%" may produce a persistent Goal.
See [RFC 0001](rfcs/0001-intent-as-first-class-domain.md).

## Data model

Defined in `crates/agentic-types` (`Goal`) and persisted through
`crates/agentic-storage`:

```text
Goal {
    id:              GoalId        // goal-<uuid>
    parent_goal:     Option<GoalId>  // builds the goal graph
    objective:       String
    rationale:       Option<String>
    constraints:     Vec<String>
    success_metrics: Vec<String>
    priority:        i32           // higher means more important
    status:          GoalStatus
    created_by:      String        // "user", "agent:planner", ...
    created_at:      Timestamp
}
```

## Status lifecycle

The model uses five statuses: `OPEN`, `IN_PROGRESS`, `COMPLETED`, `ABANDONED`
and `BLOCKED`. A new Goal starts `OPEN`; work moves it to `IN_PROGRESS`;
`COMPLETED` marks success criteria met; `ABANDONED` records a deliberate stop;
`BLOCKED` records that it cannot currently proceed.

> Implementation note: the current `crates/agentic-types::GoalStatus` enum spells
> the working states as `PROPOSED` (open), `ACTIVE` (in progress), `SUSPENDED`
> (blocked/paused), `COMPLETED`, `ABANDONED` and `FAILED`. The user-facing
> vocabulary above is the intended model; reconciling the enum name-for-name is
> planned and would require a migration.

## Decomposition

Goals decompose into a tree through `parent_goal`. A parent goal can produce
child goals, executions, experiments and generations, so a single human intent
can be broken into bounded, independently testable work. The goal graph is
queried and extended through `goald` and `agenticctl goal create|show|child|complete|abandon`.

## Relationship to executions and provenance

Executions link to the goal they served (`Execution.goal_id`); generations link
to the goal that motivated them (`Generation.reason_goal`). Every meaningful
autonomous action should be traceable to a goal, an agent and an execution. The
edges between a goal and its children or outputs are stored by `provenanced`
using relations such as `created_by`, `belongs_to` and `caused_by`.

## Implemented status

- **Implemented:** the `Goal`/`GoalStatus` model, parent/child decomposition,
  priorities, constraints, success metrics, persistence across service restarts,
  and storage CRUD used by `goald`.
- **Planned:** automatic status transitions driven by evidence, success-criteria
  evaluation, goal scheduling across multiple agents, and budget-bound goals.
