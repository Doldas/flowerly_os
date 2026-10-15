# 0013: Conceptual Domains Do Not Automatically Become Daemons

Date: 2026-10-11  
Status: Accepted

## Context

Flowerly OS has many durable conceptual domains: Flows, Goals, Sessions,
Provenance, Generations, Memory, Intent, Evolution and more. Treating every
domain name as a mandatory process boundary would turn a single-machine OS core
into an unnecessary microservice architecture.

## Decision

A conceptual service/domain boundary does not imply a Unix process boundary.

A new daemon requires at least one concrete reason such as:

- privilege separation
- fault isolation
- independent lifecycle or restart semantics
- a distinct trust boundary
- resource accounting/isolation
- materially different runtime/toolchain requirements

If those reasons do not exist, prefer a Rust crate/module behind an explicit
interface.

The current bootstrap daemons remain valid while their boundaries are evaluated
through implementation evidence; this ADR prevents future daemon proliferation
by naming convention alone.

## Consequences

- Architecture documents may describe more conceptual components than running
  processes.
- Refactoring a daemon into a library, or a library into a daemon, is allowed
  when evidence changes.
- Local calls should not become IPC merely to preserve a diagram.
- Security boundaries should be real Linux boundaries, not decorative service
  names.
