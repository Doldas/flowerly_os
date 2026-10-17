# 0020: AI Compute Layer Is a flowerlyd Module Until Enforcement Evidence Exists

Date: 2026-10-17  
Status: Accepted

## Context

RFC 0004 (location-independent Flowerly and AI compute profiles) names five
interfaces to investigate: a runtime-facing client protocol, an inference
provider boundary, a model catalog / compatibility checker, an AI resource
scheduler, and remote runtime / fleet management. Issue #64 tracks the planning
deliverables and its nonnegotiable constraints: nothing may claim remote
runtimes, providers, web clients or AI schedulers exist; security and action
authority remain OS-enforced, never model- or profile-selected.

ADR-0013 requires a concrete justification before creating a daemon. The
local compute tranche (model catalog, profiles, provider registry borders,
admission bookkeeping) currently has none of those reasons: it needs no
privilege separation beyond the existing single-user supervisor, no distinct
trust boundary (its data is user-level preference and evidence), and no
independent lifecycle.

## Decision

The AI compute layer — model catalog, expert profiles, provider registry
boundary and workload admission/usage ledger — is implemented as
**flowerlyd modules behind explicit interfaces** (`flowerlyd::compute`),
sharing the durable unified SQLite state per ADR-0011 and the local
authenticated Unix-socket IPC per ADR-0010. No new daemon, no network
socket, no protocol version change.

Promoting any part of this layer into its own process requires the same
evidence ADR-0013 lists (for example: verified per-process enforcement of
inference resources via Linux cgroups/GPU controls, or a provider serving
external clients). Until evidence exists, capability tiers, profile
ceilings, provider registrations and admission decisions are preference and
accounting data with honest `advisory-only` labels; none of them grant OS
authority, and GPU/accelerator workloads are refused until a verified VRAM
measurement and enforcement path exists.

Remote provider kinds and any egress stay structurally refused
(`PermissionDenied`) until an authorized egress policy gate with explicit
review exists (RFC 0004 threat model: credential theft, accidental egress,
denial of service; trust model doc: "AI providers are replaceable and never
trusted with root").

## Consequences

- The compute layer ships with the unified supervisor and follows its
  update/restart/recovery story.
- No inference execution, scheduling preemption, or web client ships with
  this ADR; later tranches must earn daemon boundaries with evidence.
- Honest limitations remain documented in
  `web-docs/docs/compute-{catalog,profiles,scheduling}.md`.
