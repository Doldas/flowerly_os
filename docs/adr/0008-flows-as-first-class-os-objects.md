# 0008: Flows as First-Class Persistent OS Objects

Date: 2026-10-11
Status: Accepted

## Context

Users reconstruct context constantly: reopening terminals, files, tools and
services for the same activity. Flowerly OS instead treats a coherent human
activity context (development, art, server operations, gaming) as a durable
object the user switches between rather than rebuilds. Context must survive
window closes and service restarts to be useful.

## Decision

A Flow is a first-class persistent operating-system object, not a UI concept.
Flows have stable `FlowId`s and durable records, and are created, listed,
activated and archived through `flowd` and `agenticctl`. The General Flow always
exists: `flowd` ensures it on startup and it is the safe default context.
Sessions, Goals and provenance edges attach to Flows through `FlowId`, and
provenance records who created each Flow (`created_by`). Flow records survive
service restarts like any other durable object.

## Consequences

- Context switching replaces context reconstruction; Flows become the unit of
  user-facing structure.
- Every dependent object (sessions, goals, provenance) can be scoped to a Flow.
- Flows add schema, lifecycle and policy surface that must stay coherent.
- Real runtime isolation per Flow (cgroups, namespaces) is planned, not yet done.
- Implemented today: the Flow model, General Flow, and persistence in `flowd`.
