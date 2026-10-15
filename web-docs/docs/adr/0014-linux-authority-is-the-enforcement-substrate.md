# 0014: Linux Authority Is the Enforcement Substrate

Date: 2026-10-11  
Status: Accepted

## Context

Flowerly OS introduces capabilities so autonomous agents can receive explicit,
scoped and attributable authority. That layer must not accidentally become a
parallel operating system in which every ordinary Linux operation is proxied
through Flowerly-specific APIs.

Linux already provides the real mechanisms that govern process authority and
contain consequences:

- users and groups
- file permissions and ACLs
- process credentials
- namespaces
- cgroups
- seccomp
- capabilities in the Linux-kernel sense
- sockets
- mount and device permissions
- systemd service boundaries
- ordinary executable and filesystem semantics

Flowerly capabilities solve a different problem: they describe who may delegate
which class of action to which actor, under what scope, and with what provenance.

## Decision

Flowerly OS capability policy is a **delegation and attribution layer above
Linux authority**, not a replacement for Linux authority.

A capability decision may authorize the system to construct an execution
environment. That environment must then receive real Linux credentials and
containment appropriate to the decision.

Once legitimately delegated, normal work should use ordinary Linux interfaces
directly. Flowerly OS must not require all file access, process execution,
networking, IPC, mounts, or device interaction to pass through
`capabilityd` as a syscall proxy.

The effective permission for an action is therefore constrained by both:

```text
Agentic delegation / policy
            ∩
real Linux authority / containment
```

A Flowerly capability can never grant authority that the resulting Linux
execution context does not actually possess.

## Consequences

- Agents remain ordinary Linux actors with explicit identities and scopes.
- Existing Linux tooling stays usable by humans and agents.
- `capabilityd` can remain focused on policy, delegation, explanation and
  provenance rather than becoming an enormous privileged broker.
- Security boundaries must ultimately correspond to real enforcement
  mechanisms, not only metadata records.
- Evolution World can grant broader authority because its consequence domain is
  isolated by KVM and related Linux boundaries.
- High-level Agentic APIs remain useful conveniences, but they are not the only
  legal path to the operating system.
