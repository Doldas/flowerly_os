# Roadmap

The execution plan mirrors the GitHub milestones. Flowerly OS is ambitious, so
the implementation strategy is deliberately conservative.

> **Central engineering rule:** build one real primitive at a time. Every stage
> must be useful, testable, inspectable, and capable of becoming part of the
> final system. Do not jump ahead.

Status legend: done, in progress, planned.

## Future direction — location-independent Flowerly and AI compute (proposed)

[**RFC 0004**](rfcs/0004-location-independent-flowerly-ai-compute.md) records a long-term vision in which the user's interface, persistent Flowerly runtime and AI inference can be placed on different devices or hosts. A local workstation, a browser/thin-client tablet, a dedicated Flowerly VM, organizational model servers and permitted external APIs are **deployment choices**, not claims about implemented capabilities. Standalone/offline Linux remains a first-class configuration.

User-friendly AI capability profiles (**Essential / Enhanced / Advanced / Custom**) should be separate from background-activity preferences. Expert users should be able to author portable, versioned per-agent model/resource profiles. Organizations may optionally centralize model registries, policy and inference, subject to user/tenant isolation and independently enforced OS authority.

**No new 0.0.4 exit criteria:** this is exploratory future work. Do not confuse model inference placement with runtime/process ownership, browser clients with VMs, or an authenticated remote connection with local Unix-socket peer credentials. See the linked RFC for proposed milestones, threat model and open questions.

## Distribution substrate direction

Arch Linux is the selected initial userspace/package substrate (ADR-0015). Its
rolling repositories are treated as candidate-generation inputs, not as a
command to mutate Stable World directly. The first concrete scaffold lives in
`image/arch/`.

The roadmap therefore separates two concerns:

```text
Arch rolls continuously
        ↓
candidate generation construction/testing
        ↓
Flowerly Stable World changes deliberately
```

Bootable-image work still belongs to 0.0.9; choosing the substrate now lets the
Generation and Evolution milestones design against the real package model
instead of an abstract future distribution.

## 0.0.1 — Repository Foundation (done)

- **Goal:** a professional repository that supports long-term human and agent
  development.
- **Key deliverables:** git repository on `main`; license; Rust workspace;
  repository layout; root documentation; `justfile`; CI; issue/PR templates;
  labels; milestones; the first architecture ADRs.
- **Entry criteria:** project owner has chosen the name, license, and default
  branch.
- **Exit criteria:** `just check` and `just test` succeed from a clean clone; CI
  is green; the repository explains the project.

## 0.0.2 — Core Runtime Skeleton (done)

- **Goal:** shared Rust foundations used by every service.
- **Key deliverables:** `agentic-types`, `agentic-storage`, `agentic-ipc`,
  `agentic-core`; typed IDs; structured config; tracing; one shared local
  SQLite state database with concurrent-safe migrations; Unix-socket IPC; a
  daemon lifecycle helper; minimal `agenticd`, `flowd`, `sessiond`, `goald`.
- **Entry criteria:** 0.0.1 merged.
- **Exit criteria:** all daemons start and stop cleanly; migrations run; IPC
  request/response works; no daemon needs AI or network; an integration test
  starts a service and queries status.

## 0.0.3 — Persistent State (done)

- **Goal:** make the system remember durable objects.
- **Key deliverables:** migration-backed CRUD for Flow, Goal, Session,
  Execution, Generation, and ProvenanceEdge; the `agenticctl` admin CLI;
  validated typed provenance endpoints with incoming/outgoing lookup;
  transactional generation activation; provenance relationships
  (`created_by`, `caused_by`, `belongs_to`, `produced`, `tested_by`,
  `promoted_to`).
- **Entry criteria:** 0.0.2 runtime and migrations in place.
- **Exit criteria:** state survives restart; migrations are tested; object IDs
  are stable; provenance edges are queryable; corruption is not silently
  ignored.

## 0.0.4 — Flows and Sessions (in progress)

- **Goal:** the first distinctive Flowerly OS primitive, built on a runtime safe
  enough to own real PTYs and processes.
- **Entry hardening gate:** safe PR/workflow trust boundary; coherent
  release-promotion rules; disposable PR runners; repaired CLI/self-test/Arch-VM
  paths; exclusive socket/process ownership; bounded IPC with deadlines;
  blocking SQLite isolated from Tokio workers; explicit local peer identity;
  dynamic readiness/status; machine-readable errors for touched APIs.
- **Flow deliverables:** stable General Flow semantics; unambiguous lookup;
  atomic foreground activation in the user/state scope; `last_active` and
  revision updates in the same transaction; concurrency tests.
- **Session deliverables:** persistent PTY ownership whose runtime lifecycle is
  independent from ATTACHED/DETACHED presentation state; attach, detach,
  reattach, resize, bounded output buffering, terminate/exit observation, Flow
  association and safe process-group identity.
- **Entry criteria:** persistent state (0.0.3) plus the review-driven runtime
  hardening gates.
- **Exit criteria:** a black-box integration test proves create Flow, start a
  real shell, run a long process, detach, switch Flow, reattach, and find the
  same process alive under the same Session id; foreground/runtime ownership
  invariants also hold.
- **Detailed plan:** [Review-driven implementation plan](review-implementation-plan-2026-10-12.md).

## 0.0.5 — Generations

- **Goal:** a reproducible model for system state and rollback.
- **Key deliverables:** Generation manifest; create/list/inspect/test/promote/
  activate/rollback semantics; integration with source commit, build manifest,
  parent generation, reason, provenance and activation history; exact resolved
  Arch package/kernel/build identities; schema/recovery guards; and a minimal
  bootable candidate fixture suitable for 0.0.6 KVM testing. This fixture is
  test infrastructure, not the distribution-grade 0.0.9 image.
- **Entry criteria:** Flows and sessions plus provenance (0.0.3–0.0.4).
- **Exit criteria:** manifests are reproducible; a candidate can be created;
  activation is recorded; rollback returns a known previous state; history
  survives reboot.

## 0.0.6 — Evolution Lab

- **Goal:** build and test candidate generations inside KVM automatically.
- **Key deliverables:** `evolution/vm`, `evolution/benchmarks`, and
  `evolution/promotion`; automated candidate build to KVM boot, health test,
  integration test, shutdown, and result artifact; Experiment records.
- **Entry criteria:** generations (0.0.5) and a buildable image.
- **Exit criteria:** `just vm-test` builds a candidate, boots it, verifies a
  service, exits, and records success or failure.

## 0.0.7 — Goal Runtime

- **Goal:** turn persistent Goals into an execution/runtime contract.
- **Key deliverables:** legal Goal transitions, decomposition/parent-child
  semantics, constraints, dependencies/scheduling, success evidence, and
  Goal/provenance links to executions, experiments and generations. Basic Goal
  persistence/CRUD already exists and is not reimplemented here.
- **Entry criteria:** provenance and generations (0.0.5).
- **Exit criteria:** a goal can produce a child goal, execution, experiment, and
  generation relationship.

## 0.0.8 — Agent Runtime

- **Goal:** bounded autonomous agents operating the existing architecture.
- **Key deliverables:** Agent model (role, goal, capabilities, status); a
  provider-neutral model interface; initial tools (read file, write workspace
  file, run command, query state, query goals, create child goal); every action
  produces an Execution record.
- **Entry criteria:** goal runtime, working Evolution World, authenticated IPC
  context, real capability/delegation policy, and bounded execution.
- **Exit criteria:** an agent can receive a goal, inspect a test project, modify
  source, run tests, record an execution, and produce a finding without losing
  provenance.

## 0.0.9 — Minimal Agentic Environment

- **Goal:** a distribution-grade Flowerly OS image, still without autonomous
  self-modification. Earlier milestones may boot minimal test fixtures; this
  milestone turns them into an operational system image.
- **Key deliverables:** reproducible Arch Linux root filesystem; initramfs;
  systemd; networking; users; a VM-first installable image carrying the
  Flowerly services and `agenticctl`; exact resolved Arch package manifest;
  General Flow present; journald logs; a documented recovery path.
- **Entry criteria:** the Evolution Lab can boot candidates (0.0.6).
- **Exit criteria:** a clean build from source produces a bootable KVM image
  with the services running; its resolved Arch package/kernel inputs are
  recorded; rebuilding from the recorded generation inputs is demonstrably
  possible or any remaining reproducibility limitation is explicit.

## 0.1.0 — First Real Agentic OS

- **Goal:** the first coherent, real Flowerly OS development release.
- **Key deliverables:** real Linux boot with Flowerly OS services; persistent
  state, General Flow, persistent sessions, goals, provenance, generations, and
  rollback; the Evolution World; a provider-neutral agent runtime; and one
  bounded self-development demonstration using the same pipeline humans use.
- **Entry criteria:** 0.0.3 through 0.0.9 complete.
- **Exit criteria:** release notes, a build manifest, and a test summary
  accompany the tag; boot, rollback, and provenance tests pass; one agent-made
  change is demonstrated in KVM and recorded through provenance.

## Post-0.1 (direction only)

0.2 Flow OS, 0.3 Adaptive Agentic Shell, 0.4 Ambient Interaction, 0.5 User
Model, 0.6 Autonomous Evolution, 0.7 Multi-machine Agentic OS, and eventually
1.0: a daily-drivable, self-evolving Flowerly OS. Exact scope is determined from
the evidence accumulated before then.
