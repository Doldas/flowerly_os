# Roadmap

The execution plan mirrors the GitHub milestones. Flowerly OS is ambitious, so
the implementation strategy is deliberately conservative.

> **Central engineering rule:** build one real primitive at a time. Every stage
> must be useful, testable, inspectable, and capable of becoming part of the
> final system. Do not jump ahead.

Status legend: done, in progress, planned.

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

## 0.0.2 — Core Runtime Skeleton (mostly done)

- **Goal:** shared Rust foundations used by every service.
- **Key deliverables:** `agentic-types`, `agentic-storage`, `agentic-ipc`,
  `agentic-core`; typed IDs; structured config; tracing; one shared local
  SQLite state database with concurrent-safe migrations; Unix-socket IPC; a
  daemon lifecycle helper; minimal `agenticd`, `flowd`, `sessiond`, `goald`.
- **Entry criteria:** 0.0.1 merged.
- **Exit criteria:** all daemons start and stop cleanly; migrations run; IPC
  request/response works; no daemon needs AI or network; an integration test
  starts a service and queries status.

## 0.0.3 — Persistent State

- **Goal:** make the system remember durable objects.
- **Key deliverables:** migration-backed CRUD for Flow, Goal, Session,
  Execution, Generation, and ProvenanceEdge; the `agenticctl` admin CLI;
  validated typed provenance endpoints; provenance relationships
  (`created_by`, `caused_by`, `belongs_to`, `produced`, `tested_by`,
  `promoted_to`).
- **Entry criteria:** 0.0.2 runtime and migrations in place.
- **Exit criteria:** state survives restart; migrations are tested; object IDs
  are stable; provenance edges are queryable; corruption is not silently
  ignored.

## 0.0.4 — Flows and Sessions

- **Goal:** the first distinctive Flowerly OS primitive.
- **Key deliverables:** General Flow plus create/rename/list/activate/archive;
  states FOREGROUND/ACTIVE/ARCHIVED; persistent PTY sessions whose runtime
  lifecycle is independent from ATTACHED/DETACHED presentation state; attach,
  detach, reattach, terminate, and Flow association; CLI operations.
- **Entry criteria:** persistent state (0.0.3).
- **Exit criteria:** an integration test proves create Flow, start a persistent
  shell, run a long process, detach, switch Flow, reattach, and find the process
  still alive.

## 0.0.5 — Generations

- **Goal:** a reproducible model for system state and rollback.
- **Key deliverables:** Generation manifest; create/list/inspect/activate/
  rollback; integration with source commit, build manifest, parent generation,
  reason, and provenance; initially over a constrained test root filesystem.
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

- **Goal:** persistent goals as operating-system objects.
- **Key deliverables:** Goal model (parent, objective, rationale, constraints,
  success metrics, priority, status); create/show/child/complete/abandon;
  goal/provenance links to executions and generations.
- **Entry criteria:** provenance and generations (0.0.5).
- **Exit criteria:** a goal can produce a child goal, execution, experiment, and
  generation relationship.

## 0.0.8 — Agent Runtime

- **Goal:** bounded autonomous agents operating the existing architecture.
- **Key deliverables:** Agent model (role, goal, capabilities, status); a
  provider-neutral model interface; initial tools (read file, write workspace
  file, run command, query state, query goals, create child goal); every action
  produces an Execution record.
- **Entry criteria:** goal runtime and the capability service.
- **Exit criteria:** an agent can receive a goal, inspect a test project, modify
  source, run tests, record an execution, and produce a finding without losing
  provenance.

## 0.0.9 — Minimal Agentic Environment

- **Goal:** a bootable Flowerly OS image, still without autonomous
  self-modification.
- **Key deliverables:** reproducible root filesystem; initramfs; systemd;
  networking; users; a VM-first installable image carrying the services and
  `agenticctl`; General Flow present; journald logs; a documented recovery path.
- **Entry criteria:** the Evolution Lab can boot candidates (0.0.6).
- **Exit criteria:** a clean build from source produces a bootable KVM image
  with the services running.

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
