# 0012: agenticctl Is a Thin Human/Agent Control Plane

Date: 2026-10-11  
Status: Accepted

## Context

Humans, tests, future graphical shells and autonomous agents need a common way to
operate the local Agentic runtime. Putting privileged logic in a special CLI
would create a second system architecture.

## Decision

`agenticctl` is a thin client over the same local IPC contracts used by other
presentations.

It may format input/output for humans, but domain authority and privileged
behavior remain in the system services and Linux execution boundaries.

## Consequences

- Humans and agents exercise the same control contracts.
- Future GUI/voice interfaces do not need a privileged parallel API.
- CLI behavior remains easy to test and automate.
- Business or security logic added only to `agenticctl` is an architecture bug.
