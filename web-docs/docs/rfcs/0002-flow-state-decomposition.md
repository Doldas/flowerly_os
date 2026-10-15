# RFC 0002: Decompose Flow Identity from Runtime and Presentation State

Status: Proposed

## Problem

The bootstrap `Flow` record is intentionally small. If every future concern is
added directly to that record it will eventually mix durable identity,
configuration, runtime processes, UI presentation and security policy into one
large object.

## Proposal

Keep `Flow` as the stable identity/root aggregate and evolve additional state
as separately versioned concerns:

```text
Flow
├── identity / purpose / hierarchy
├── FlowConfig revision
├── FlowRuntime state
├── FlowPresentation state
└── FlowPolicy references
```

Examples:

- **FlowConfig:** declared services, project associations, defaults.
- **FlowRuntime:** active sessions, resource state, transient lifecycle.
- **FlowPresentation:** layout, visible surfaces, interaction style.
- **FlowPolicy:** credential scope, notifications, resource/security policy.

The exact structs are intentionally not frozen yet.

## Why now

This records the separation before 0.0.4 adds real persistent PTY sessions and
before the adaptive shell begins adding presentation state.

## Non-goals

- Do not add dozens of empty tables now.
- Do not require every concern to be a daemon.
- Do not make a Flow a container or namespace by definition.
