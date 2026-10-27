# Flowerly OS — Threat Model

Flowerly OS runs autonomous agents and evolves itself. That makes its threat
model larger than a conventional desktop: the system must stay safe when a model
is wrong, compromised or hostile, when external content is adversarial, and when
an experiment fails badly. This document states the trust principles and the
threats being tracked. It is a starting point, not a solved problem.

## Trust principles

1. **AI providers are replaceable and never trusted with root.** A provider is an
   untrusted advisor. It cannot directly perform privileged operations.
2. **Boot and administration never depend on AI.** The system boots, logs in and
   is administrable with no model, network or credentials available.
3. **Authority is explicit.** Nothing happens without a granted capability and,
   for autonomous changes, a provenance record.
4. **Autonomy is bounded.** Actions are limited by capabilities, goal bounds and
   budgets; there is no unrestricted autonomous root modification.
5. **Experiments are sandboxed.** Aggressive changes run in the Evolution World
   (KVM), so they cannot harm the Stable World.
6. **Everything important is recorded.** Provenance is the audit trail and the
   basis for reversal decisions.

## Threat categories

| Threat | Notes | Primary mitigations |
| --- | --- | --- |
| Compromised model provider | A provider returns malicious or manipulative output | Provider treated as untrusted data; capabilities gate actions; provenance records requests |
| Malicious tool output | Tool results carry injected instructions | Treat tool output as data, not commands; capability checks at the boundary |
| Prompt injection from external content | Web pages, files or messages try to redirect an agent | Separate instructions from data; least-authority bindings; explicit user intent |
| Privilege escalation | An agent or flow gains authority it was not granted | Capability model; delegation only downward; no root capability in the catalog |
| Poisoned package | A dependency or package is malicious | Pinned, minimal dependencies; audits (`cargo audit`, `cargo deny`) as they stabilize; supply-chain review |
| Corrupted generation | A generation is damaged or tampered with | Reproducible builds, manifest hashes, provenance from source to generation |
| Credential leakage | Secrets reach logs, providers or other flows | Secrets policy below; Flow credential scoping (planned) |
| Agent confused deputy | An agent is tricked into using its authority for a third party | Explicit intent and goal binding; provenance; capability scoping |
| Persistent malicious memory | Poisoned memory influences future behavior | Explicit/observed/inferred separation; never promote inference to fact; invalidatable records |
| Unsafe Flow credential crossover | Credentials from one Flow leak into another | Per-Flow credential scope (planned); no ambient authority across Flows |

## Limiting autonomous action

- **Capabilities:** agents hold only the capabilities they were granted; see
  [capabilities.md](capabilities.md). The initial catalog has no root-modifying
  capability.
- **Goal bounds:** an agent acts within a goal's constraints, success criteria
  and priority; work outside the goal is out of scope.
- **Budgets:** resource, time and cost budgets bound how far an agent or
  experiment may go (planned).
- **Sandbox:** potentially destructive work runs in the Evolution World, not the
  Stable World.

## Provenance as audit trail

Every meaningful autonomous action must be attributable. Provenance answers who
did what, why, under which goal, agent, model, command, source, test and
generation. See [provenance.md](provenance.md). An action that cannot be
recorded should not happen autonomously.

## Supply chain

Dependencies are audited, pinned and kept minimal. The Rust workspace uses a
locked dependency graph (`Cargo.lock`) and `rusqlite` with the `bundled` feature
so the SQLite version is fixed at build time. `cargo audit` and `cargo deny` are
introduced once the dependency surface stabilizes. Builds are intended to be
reproducible so a generation's manifest can be verified.

## Secrets policy

- Secrets are never committed to the repository and never written to logs or
  provenance.
- Provider credentials are configuration, not architecture: they are not mixed
  into core services and their absence must not break the system.
- Credentials are scoped to a Flow rather than shared globally (planned).
- Model providers must not receive secrets they do not need.

## Implementation status

Documented today and enforced in part: no-AI boot, the capability catalog, and
provenance for durable objects. Local IPC administration is enforced by
kernel-verified Linux group membership (`flowerly-admin`); PTY shells are
limited to the service account. Users of one Flowerly instance would share one
data namespace, so the shipped configuration admits only the service account
until per-user ownership lands (#225, [command authorization](command-authorization.md)).
**Planned:** per-user data ownership, policy-based authorization, budgets,
Flow credential scoping, dependency audits in CI, reproducible verified builds,
and the Evolution World sandbox itself.
