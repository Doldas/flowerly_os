# Flowerly OS — Capabilities

Capabilities are the units of authority in Flowerly OS. An agent (or Flow, or
user session) may perform an action only if it holds the matching capability.
The default is no authority: agents start with nothing and must be explicitly
bound to what they need. This is the primary limit on autonomous action.

## Capability model

A capability is a named grant, defined in `crates/agentic-types` (`Capability`)
and catalogued by `capabilityd`:

```text
Capability {
    name:        String        // e.g. "file.read"
    description: Option<String>
    granted:     bool          // whether it is currently granted
}
```

Capabilities are checked at the boundary of every privileged operation: before
an agent reads a file, runs a process, creates a child goal or launches an
experiment, `capabilityd` is consulted.

## Initial catalog

The initial catalog is the ten entries shipped in
`services/capabilityd/src/main.rs`:

```text
file.read                    read files
file.write_workspace         write files inside a workspace
process.run                  run commands/processes
system.query_state           query system state
goal.query                   read goals
goal.create_child            create a child goal
session.query                read sessions
provenance.query             read provenance
generation.create_candidate  create a candidate generation
evolution.run_experiment     run an experiment in the Evolution World
```

They are exposed through `agenticctl capability` (the `capability.list`
operation). The catalog is deliberately narrow: there is no unrestricted
root-modification capability.

## Agents must be bound to capabilities

An agent is bound to a set of capabilities for the duration of its work and
cannot escalate beyond that set. Authority can be delegated downward but never
silently upward: a child agent receives a subset of its parent's capabilities.
There is no ambient authority inherited from being "the agent layer".

## Policy and authorization (planned)

Policy evaluation and authorization are **planned, not implemented**;
`capabilityd` currently exposes the catalog and a policy-evaluation stub. The
intended design:

- Decision: given an actor, a capability and a request context, return allow or
  deny with a reason.
- Scope: capabilities may be limited to a Flow, project, workspace path or time
  window.
- Audit: every decision and privileged action produces a provenance edge and an
  Execution record.

Until policy exists, autonomous privileged action is not permitted; the system's
own services perform their bounded operations directly and record provenance.
See [threat-model.md](threat-model.md): AI providers are replaceable and never
trusted with root, so authority stays explicit, bounded and recorded.
