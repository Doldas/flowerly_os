# Flowerly OS — Capabilities

Capabilities are Flowerly OS's explicit vocabulary for identity, delegated
authority, scope and provenance. They complement Linux authority rather than
replacing it.

An agent (or Flow, or user session) may request an Agentic privileged action only
when it holds the matching capability. The default is no delegated Agentic
authority: agents start with nothing and are explicitly bound to what they need.

Linux remains the execution substrate and final enforcement layer for many real
operations through process identity, file permissions, namespaces, cgroups,
seccomp, sockets and other kernel/userspace mechanisms. This layering is an
accepted architectural decision; see
[ADR-0014](adr/0014-linux-authority-is-the-enforcement-substrate.md).

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

Capabilities are checked at Agentic privilege/delegation boundaries: before an
agent is given authority to read a workspace, launch a process, create a child
goal, or start an experiment, policy can evaluate the requested capability and
scope.

This must **not** turn `capabilityd` into a proxy for every Linux syscall. Once
an execution is legitimately delegated, the process should use ordinary Linux
interfaces directly according to its real OS credentials and containment.

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
Conversely, a capability grant is not magic permission that bypasses Linux: the
execution environment must also possess the corresponding real system
authority.

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
