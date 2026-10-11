# Flowerly OS — Session Model

A terminal session is not a window. The window (or any other presentation) is a
view onto a persistent Session that survives the presentation closing. Sessions
are durable system objects owned by `sessiond`, so a user can later ask to
reattach to the terminal where a long build is still running.

## Data model

Defined in `crates/agentic-types` (`Session`) and persisted through
`crates/agentic-storage`:

```text
Session {
    id:         SessionId      // session-<uuid>
    flow_id:    Option<FlowId> // the Flow this session belongs to
    kind:       String         // session kind, see below
    pid:        Option<i32>    // OS process id while running
    cwd:        Option<String> // working directory the session started in
    state:      SessionState
    created_by: String         // "user", "agent", ...
    created_at: Timestamp
}
```

## States

The intended lifecycle is `ACTIVE`, `SUSPENDED` and `EXITED`: a session is
`ACTIVE` while its process tree runs (attached or detached), `SUSPENDED` while
its process is stopped or frozen, and `EXITED` once it terminates.

> Implementation note: the current `crates/agentic-types::SessionState` enum uses
> `RUNNING`, `EXITED` and `DETACHED`. In practice `DETACHED` corresponds to an
> `ACTIVE` session with no attached presentation. Reconciling the names is
> planned; the distinction between "alive but detached" and "exited" is the
> important one and is already modelled.

## Session kinds

Sessions are typed by `kind`. The intended vocabulary is:

```text
local-pty   a local pseudo-terminal running a shell or process tree
ssh         a remote session reached over SSH
container   a console into a container
vm-console  a console into a virtual machine
```

`kind` is currently a free-form string on the record; the vocabulary above is the
target and will be validated when the real session backends land.

## Persistence and Flow association

Session records are durable and survive service restart. Each session optionally
belongs to a Flow through `flow_id`, which is how a Flow gathers "its"
terminals. Closing a presentation detaches it from the session; it does not
terminate the process tree.

## Implementation status

- **Implemented:** the `Session`/`SessionState` model, persistence across
  restarts, Flow association, state transitions and storage CRUD in `sessiond`.
- **Not implemented:** real PTY allocation, attach/detach/reattach plumbing and
  process-tree survival. PTY reattachment is the Sprint 3 vertical slice and is
  explicitly **not implemented** yet.
- **Planned:** SSH/container/VM-console backends, session reconnect ergonomics
  and resource accounting per session.
