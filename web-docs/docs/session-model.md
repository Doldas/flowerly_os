# Flowerly OS — Session Model

A terminal session is not a window. The window (or another presentation) is a
view onto a persistent Session. Closing that presentation must not imply that the
underlying shell/process exits.

## Two independent state dimensions

A Session has two orthogonal state machines.

### Runtime state

```text
RUNNING
SUSPENDED
EXITED
```

- `RUNNING` — the underlying session/process is alive.
- `SUSPENDED` — execution is intentionally stopped/frozen but resumable.
- `EXITED` — the underlying session has terminated.

### Presentation state

```text
ATTACHED
DETACHED
```

- `ATTACHED` — at least the current presentation is attached to the session.
- `DETACHED` — no presentation is attached; the runtime may still be RUNNING.

This separation is essential to the Flowerly OS model:

```text
RUNNING + DETACHED
```

is the normal state of a persistent terminal after its window disappears.

## Data model

Defined in `crates/agentic-types` (`Session`) and persisted through
`crates/agentic-storage`:

```text
Session {
    id:                 SessionId
    flow_id:            Option<FlowId>
    kind:               String
    pid:                Option<i32>
    cwd:                Option<String>
    state:              SessionState
    presentation_state: SessionPresentationState
    created_by:         String
    created_at:         Timestamp
}
```

Migration 0002 converts legacy `DETACHED` runtime rows to
`RUNNING + DETACHED`.

## Session kinds

The intended vocabulary is:

```text
local-pty   local pseudo-terminal running a shell/process tree
ssh         remote SSH session
container   console into a container
vm-console  virtual-machine console
```

`kind` remains a free-form string during bootstrap; validation lands with the
real backends.

## Persistence and Flow association

Session records are durable and optionally belong to a Flow through `flow_id`.
The records live in the shared Flowerly `state.db`, so Flow foreign keys and
session relationships represent real local state rather than copies in separate
service databases.

## Implementation status

- **Implemented:** durable Session identity, Flow association, independent
  runtime/presentation state, migration compatibility, storage CRUD and
  `sessiond` metadata operations.
- **Not implemented:** PTY allocation, process-tree ownership and real
  attach/detach/reattach plumbing.
- **Planned for 0.0.4:** prove the vertical slice: create Flow → start shell →
  detach presentation → switch Flow → return → reattach to the same live
  process.
- **Later:** SSH/container/VM-console backends and resource accounting.
