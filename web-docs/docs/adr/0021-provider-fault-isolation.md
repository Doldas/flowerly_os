# 0021: Reference Inference Provider Runs as a Fault-Isolated Process

Date: 2026-10-17  
Status: Accepted

## Context

RFC 0004 interface 2 (inference provider boundary) requires capability
discovery, request identity, bounded results, accounting, and honest outage
behavior. ADR-0013 requires a concrete justification before creating a
daemon. The reference provider (`providerd`) is the first process candidate
beyond the two existing daemons.

## Decision

`providerd` is a **separate process** speaking the same authenticated local
Unix-socket IPC as `flowerlyd` (ADR-0010), justified by ADR-0013's
**fault isolation** criterion: inference providers are untrusted, potentially
long-running and crash-prone; an outage must never corrupt or block the
Flowerly runtime. The reference provider is synthetic, bounded and
fail-closed; it writes no durable state and cannot grant OS authority.

The provider is **not** added to the distribution image manifest
(`distro-builder` validation keeps its exact binary list) until real
provider deployments are authorized; it is dev/CI infrastructure today.

## Consequences

- Provider outages surface as typed `Unavailable` in flowerlyd; the profile
  fallback policy (`ask`/`queue`/`fail`) decides the honest response, and
  `downgrade` is refused outright (RFC 0004 forbids silent substitution).
- The only durable artifact of an execution is the bounded usage-ledger row
  written by flowerlyd after an OK provider result; retries replay it
  (#63 semantics).
- A future real provider must satisfy the same contract (bounded, local
  socket, no egress, no credentials) or earn a new ADR with evidence.
