# Agentic OS — Repository Bootstrap, Architecture & Release Roadmap

> **Purpose:** This document is intended to be handed directly to OpenCode or another autonomous coding agent to bootstrap the Agentic OS GitHub repository, establish the project architecture, and execute the initial development roadmap as a sequence of sprints and release builds.

**Project:** Agentic OS  
**Type:** Self-evolving Linux distribution  
**Primary languages:** Rust, Go, Python, C, Java where justified  
**Kernel strategy:** Upstream Linux, no custom kernel fork for the initial architecture  
**Development strategy:** Small real system, reproducible builds, persistent architecture, automated tests, generations, rollback, KVM-based evolution lab  
**User experience strategy:** Intent-first interaction, Ambient Interaction, persistent Flows, adaptive interfaces, progressive abstraction  
**Repository philosophy:** Humans and agents use the same source tree, build system, tests, issue tracker, CI, release process, and provenance

---

# 1. Mission

Agentic OS is a Linux distribution designed around autonomous agents as first-class system actors.

The operating system should be capable of:

- understanding user intent
- maintaining persistent goals
- maintaining durable memory
- observing its own behavior
- operating Linux directly
- developing software
- testing candidate changes
- building new OS generations
- evaluating experiments
- rolling back failed changes
- evolving its own userspace
- adapting its interaction model to the user
- maintaining persistent activity contexts called Flows
- eventually becoming substantially different for different users

The project must begin as a real operating system project rather than a disposable AI demonstration.

The first versions may be minimal, but the architecture should be designed to survive.

---

# 2. Foundational principles

## 2.1 Linux is the substrate

Use upstream Linux.

Do not begin by modifying or forking the kernel.

Agentic OS initially evolves userspace, system services, UI, agents, workflows, policy, automation, and operating abstractions.

Use normal Linux capabilities:

- systemd
- cgroups v2
- namespaces
- seccomp
- `/proc`
- `/sys`
- netlink
- sockets
- pidfds
- PTYs
- D-Bus
- PipeWire
- Wayland
- QEMU/KVM
- containers
- Git
- compilers
- ordinary package infrastructure

Structured Agentic APIs are abstractions over Linux, not replacements for Linux.

## 2.2 Agentic OS must work without an AI provider

The system must still boot and function when the internet is unavailable, an external AI provider fails, a local model fails, credentials are missing, or the agent layer is temporarily disabled.

Boot, login, recovery, storage, networking, updates, and administration must not depend on model inference.

## 2.3 Intent before operations

The user should not have to translate human intent into computer operations.

Traditional model:

```text
human goal
  ↓
application
  ↓
menu
  ↓
file
  ↓
setting
  ↓
operation
```

Agentic OS model:

```text
human goal
  ↓
intent
  ↓
context
  ↓
goal
  ↓
agents
  ↓
Linux / applications / services
  ↓
result
```

## 2.4 Context is first-class

Agentic OS introduces **Flows**.

A Flow is a persistent, evolvable operating environment representing one coherent human activity context.

Examples:

- General
- Development
- Server Operations
- Gaming
- Art
- Music
- Research
- Writing
- Project-specific environments

The user should switch contexts instead of reconstructing them.

## 2.5 Evolution is evidence-driven

```text
observe
  ↓
form hypothesis
  ↓
create goal
  ↓
design experiment
  ↓
build candidate
  ↓
test
  ↓
measure
  ↓
review
  ↓
accept / reject
  ↓
new generation
```

Failed experiments must remain useful history.

---

# 3. Language policy

## 3.1 Rust — default language for trusted OS services

Use Rust by default for:

- core daemons
- persistent session management
- Flow runtime
- goal runtime
- provenance
- capability/security services
- generation management
- privileged system integration
- long-lived local services
- system APIs
- reliability-sensitive concurrency

Examples:

```text
agenticd
flowd
sessiond
goald
provenanced
capabilityd
generationd
```

Rust is the default language for new permanent Agentic OS system software.

## 3.2 Go — distributed and orchestration services

Use Go when the problem is primarily:

- networking
- remote orchestration
- distributed systems
- synchronization
- cluster coordination
- remote execution gateways
- API gateways
- machine-to-machine communication
- lightweight infrastructure services

Potential future components:

```text
syncd
remote-execd
agent-networkd
model-router
cluster-coordinator
flow-sync
```

Do not replace Rust system services with Go without a concrete reason.

## 3.3 Python — research and rapid experimentation

Use Python for:

- AI/ML experiments
- data analysis
- benchmarks
- log analysis
- prototype agents
- migration tools
- evaluation harnesses
- build helpers
- temporary scripts
- experimental algorithms

A successful Python prototype may later become Rust or Go when it becomes permanent infrastructure.

Preferred pattern:

```text
Python prototype
    ↓
experiment proves useful
    ↓
stable architecture defined
    ↓
production implementation in Rust or Go
```

## 3.4 C — low-level integration only when justified

Use C when:

- Linux ABI interaction strongly benefits from it
- libbpf/eBPF integration requires it
- existing C libraries are the right foundation
- a very small low-level component is simpler in C
- compatibility requires C ABI

Do not use C as the default language for new Agentic OS services.

## 3.5 Java — ecosystem-specific

Java is allowed where a JVM ecosystem provides a concrete advantage.

Do not use Java as the default OS-core language.

Possible future use:

- integration with existing JVM systems
- specialized services
- tooling where JVM libraries are uniquely strong

---

# 4. High-level architecture

```text
                             HUMAN
                               │
                 voice / text / pen / touch
                               │
                               ▼
                    Ambient Interaction
                               │
                               ▼
                            Intent
                               │
                               ▼
                      Context / User Model
                               │
                               ▼
                            Goals
                               │
                  ┌────────────┼────────────┐
                  ▼            ▼            ▼
               Agents       Planner      Evolution
                  │            │            │
                  └────────────┼────────────┘
                               ▼
                      Identity / Capability
                               │
                               ▼
                      Provenance / History
                               │
             ┌─────────────────┼─────────────────┐
             ▼                 ▼                 ▼
           Flows            Projects          System
             │                 │                 │
             ▼                 ▼                 ▼
        Sessions            Apps/Tools       Services
             │                 │                 │
             └─────────────────┼─────────────────┘
                               ▼
                         Linux Userspace
                               │
                               ▼
                         Upstream Linux
                               │
                               ▼
                            Hardware
```

---

# 5. Core concepts

Agentic OS should model the following as explicit durable objects.

```text
User
Flow
Project
Goal
Agent
Session
Experiment
Observation
Finding
Decision
Artifact
Generation
Capability
Execution
```

These objects should eventually share stable IDs, timestamps, provenance, ownership, links to related objects, and revision/history information.

---

# 6. Core services

The first implementation may combine some services into fewer processes. Conceptual boundaries should remain clear.

## `agenticd`

Responsibilities:

- agent lifecycle
- agent execution
- model invocation
- tool execution
- orchestration
- delegation
- execution tracking

Language: Rust

## `goald`

Responsibilities:

- persistent goals
- parent/child goal relationships
- priorities
- constraints
- status
- success criteria
- evidence links

Language: Rust

## `flowd`

Responsibilities:

- Flow creation
- Flow lifecycle
- foreground switching
- Flow metadata
- resource policy
- Flow-specific environment
- Flow revisions
- associations with sessions/goals/projects

Language: Rust

## `sessiond`

Responsibilities:

- persistent PTYs
- terminal sessions
- SSH session metadata
- container/VM console sessions
- process attachment
- session reconnect
- durable session identity

Language: Rust

## `memoryd`

Responsibilities:

- durable memory
- structured retrieval
- user/system/project context
- explicit/observed/inferred separation
- indexing
- long-term knowledge

Initial language: Rust or Go  
Storage: SQLite first

## `observed`

Responsibilities:

- system observations
- metrics
- significant events
- service failures
- performance data
- workflow observations

Language: Rust

## `provenanced`

Responsibilities:

- causal history
- execution records
- links between goals, observations, experiments, commits, builds, and generations

Language: Rust

## `capabilityd`

Responsibilities:

- agent identity
- authority
- delegation
- policy
- capability scopes
- privileged-operation decisions

Language: Rust

## `generationd`

Responsibilities:

- system generation manifests
- candidate generations
- activation
- rollback
- generation history
- links to source/build/experiment provenance

Language: Rust

## `evolutiond`

Responsibilities:

- candidate experiments
- experiment plans
- KVM test orchestration
- benchmarks
- fitness evaluation
- promotion/rejection

Initial language: Rust  
Go may be used for distributed execution later.

## `modeld`

Responsibilities:

- provider-neutral model abstraction
- local/remote model routing
- structured requests
- tool calls
- embeddings
- model capability descriptions

Language: Rust or Go

## Future Ambient Interaction services

Possible responsibilities:

```text
ambientd
speechd
intentd
contextd
```

Do not build all of these during the earliest bootstrap sprint.

---

# 7. Flow architecture

Flows are a foundational Agentic OS primitive.

A conceptual schema:

```text
Flow {
    id
    name
    purpose
    state

    visual_environment
    interaction_model
    layout

    projects
    goals
    agents
    memory_context

    applications
    windows
    terminal_sessions

    services
    containers
    virtual_machines
    remote_sessions

    network_context
    credential_scope
    resource_policy
    notification_policy

    created_by
    created_at
    last_active

    revision
}
```

Required initial states:

```text
FOREGROUND
ACTIVE
QUIESCENT
HIBERNATED
ARCHIVED
```

The initial implementation may only fully support:

```text
FOREGROUND
ACTIVE
ARCHIVED
```

but the data model should not block later states.

---

# 8. General Flow

Every system starts with:

```text
General
```

General is always available, low configuration, neutral, suitable for temporary tasks, and the default landing context.

Users can create new Flows naturally:

```text
"Create a development flow."
"Make this project its own flow."
"Keep these terminals together."
"Create a server operations flow."
"Make what I'm doing now persistent."
```

The UI should eventually support this without requiring users to know anything about cgroups, PTYs, namespaces, or session daemons.

---

# 9. Persistent sessions

Terminal sessions are not windows.

The terminal window is a presentation of a persistent session.

```text
Session
    │
    ├── PTY
    ├── process tree
    ├── working directory
    ├── environment
    ├── Flow association
    └── presentation(s)
```

Closing a terminal window does not necessarily terminate the session.

A user may later request:

```text
"Show the terminal where the build is running."
```

The UI reconnects to the existing session.

---

# 10. Generational operating system

System changes should create reproducible generations.

Example:

```text
Generation 000012

parent:
    000011

source_commit:
    abc123

system_manifest:
    sha256:...

reason:
    G-481

experiment:
    E-104

status:
    stable
```

A candidate build should be able to move through:

```text
source
  ↓
candidate generation
  ↓
KVM boot
  ↓
tests
  ↓
benchmarks
  ↓
review
  ↓
promote / reject
```

User data must remain separate from system generation rollback.

---

# 11. Stable World and Evolution World

```text
                 PHYSICAL MACHINE

┌────────────────────────────────────┐
│            STABLE WORLD            │
│                                    │
│ generation N                       │
│ user's actual environment          │
└────────────────┬───────────────────┘
                 │
                 ▼
┌────────────────────────────────────┐
│           EVOLUTION WORLD          │
│                                    │
│ QEMU/KVM                           │
│ candidate N+1                      │
│ experiments                        │
│ destructive testing               │
└────────────────┬───────────────────┘
                 │
                 ▼
             evidence
                 │
            promote/reject
```

Evolution World is where agents may be highly autonomous.

---

# 12. Persistent storage

Start with SQLite for structured state.

Suggested logical tables:

```text
flows
projects
goals
agents
sessions
observations
findings
experiments
decisions
executions
generations
provenance_edges
user_facts
user_observations
user_inferences
artifacts
```

Use migrations from day one.

Never modify the production schema manually.

---

# 13. User model

Keep three distinct categories.

```text
EXPLICIT
user stated it directly

OBSERVED
system observed behavior

INFERRED
system generated a hypothesis
```

Never silently convert inferred information into explicit fact.

Every inferred record should eventually support confidence, source observations, timestamp, and revision/invalidated state.

---

# 14. Model abstraction

The OS must not be coupled to one vendor.

Conceptual interface:

```text
reason()
generate()
tool_call()
structured_output()
embed()
```

Model metadata should include:

```text
provider
model_id
capabilities
context_limit
local_or_remote
cost_class
latency_class
privacy_class
```

Provider credentials must not be mixed into core architecture.

---

# 15. Repository structure

Create the repository approximately as follows:

```text
agentic-os/
├── .github/
│   ├── workflows/
│   ├── ISSUE_TEMPLATE/
│   ├── PULL_REQUEST_TEMPLATE.md
│   └── CODEOWNERS
│
├── crates/
│   ├── agentic-core/
│   ├── agentic-types/
│   ├── agentic-storage/
│   ├── agentic-ipc/
│   ├── agentic-provenance/
│   ├── agentic-capabilities/
│   ├── agentic-generations/
│   ├── agentic-goals/
│   ├── agentic-flows/
│   └── agentic-sessions/
│
├── services/
│   ├── agenticd/
│   ├── goald/
│   ├── flowd/
│   ├── sessiond/
│   ├── provenanced/
│   ├── capabilityd/
│   ├── generationd/
│   └── evolutiond/
│
├── go/
│   ├── remote/
│   └── sync/
│
├── python/
│   ├── experiments/
│   ├── analysis/
│   ├── evaluation/
│   └── tools/
│
├── c/
│   └── lowlevel/
│
├── image/
│   ├── rootfs/
│   ├── initramfs/
│   ├── installer/
│   └── build/
│
├── packaging/
│   ├── systemd/
│   ├── distro/
│   └── repository/
│
├── kernel/
│   ├── config/
│   └── packaging/
│
├── shell/
│   └── README.md
│
├── evolution/
│   ├── vm/
│   ├── benchmarks/
│   ├── fitness/
│   └── promotion/
│
├── schemas/
│
├── tests/
│   ├── unit/
│   ├── integration/
│   ├── boot/
│   ├── upgrade/
│   ├── generation/
│   └── evolution/
│
├── docs/
│   ├── ARCHITECTURE.md
│   ├── LANGUAGE_POLICY.md
│   ├── FLOW_MODEL.md
│   ├── GOAL_MODEL.md
│   ├── SESSION_MODEL.md
│   ├── GENERATIONS.md
│   ├── EVOLUTION.md
│   ├── PROVENANCE.md
│   ├── CAPABILITIES.md
│   ├── USER_MODEL.md
│   ├── THREAT_MODEL.md
│   ├── BUILD.md
│   └── RELEASES.md
│
├── rfcs/
├── adr/
├── scripts/
│
├── Cargo.toml
├── Cargo.lock
├── go.work
├── rust-toolchain.toml
├── justfile
├── Makefile
├── LICENSE
├── README.md
├── CONTRIBUTING.md
├── SECURITY.md
├── CODE_OF_CONDUCT.md
├── VISION.md
├── ROADMAP.md
├── DECISIONS.md
└── JOURNAL.md
```

Keep the structure practical.

Do not create empty directories merely to satisfy this diagram unless they are part of the initial implementation or contain explanatory README files.

---

# 16. Rust workspace

Use a Cargo workspace.

Top-level `Cargo.toml`:

```toml
[workspace]
resolver = "2"
members = [
    "crates/*",
    "services/*"
]
```

Set a minimum supported Rust version intentionally.

Use:

```text
rustfmt
clippy
cargo test
cargo deny
cargo audit
```

where practical.

Avoid unsafe Rust unless required.

Every `unsafe` block must include a short safety explanation.

---

# 17. Error handling

Libraries should use typed errors and avoid panics for expected runtime failures.

Services should provide structured error context, log actionable causes, and never hide causal chains.

Do not use `unwrap()` in permanent daemon code unless the condition is logically impossible and documented.

---

# 18. Logging

Use structured logging from the beginning.

Recommended Rust approach:

```text
tracing
tracing-subscriber
```

Logs should be compatible with journald.

Important records should contain IDs such as:

```text
flow_id
goal_id
agent_id
session_id
experiment_id
generation_id
execution_id
```

Do not encode important identity only in free-form messages.

---

# 19. IPC

Initial preferred IPC:

```text
Unix domain sockets
D-Bus where natural
```

Do not invent a distributed RPC platform during Sprint 0.

Internal message schemas should be versionable.

Use serde. Prefer JSON for debug/admin interfaces until there is evidence for a more compact format.

---

# 20. systemd integration

Agentic OS services should be ordinary systemd services.

Examples:

```text
agenticd.service
flowd.service
sessiond.service
goald.service
generationd.service
```

Use systemd facilities rather than rebuilding service supervision, restart policy, dependency ordering, logging, cgroups, and socket activation.

---

# 21. Build interface

Use `just` as the primary developer command runner.

```text
just check
just test
just lint
just build
just run
just vm
just vm-test
just image
just release
```

The underlying commands may use Cargo, Go, Python, shell, and QEMU.

Do not hide build behavior behind opaque scripts.

---

# 22. Documentation model

Maintain:

## `VISION.md`

Long-term direction.

## `ARCHITECTURE.md`

Current technical architecture.

## `ROADMAP.md`

Current execution plan.

## `DECISIONS.md`

Major decisions and summaries.

## `JOURNAL.md`

Chronological engineering/evolution notes.

## `adr/`

Architecture Decision Records.

ADR filename format:

```text
0001-rust-as-primary-system-language.md
0002-upstream-linux-kernel-policy.md
0003-sqlite-as-initial-state-store.md
```

Use ADRs for consequential decisions.

---

# 23. Git strategy

Default branch:

```text
main
```

Use short-lived branches such as:

```text
feat/flow-model
feat/persistent-sessions
fix/generation-rollback
docs/ambient-interaction
```

Do not maintain permanent `develop` unless evidence later shows it is useful.

Use pull requests even for agent-generated changes once CI exists.

---

# 24. Commit policy

Use conventional-style commit prefixes:

```text
feat:
fix:
docs:
test:
refactor:
perf:
build:
ci:
chore:
```

Examples:

```text
feat(flow): add persistent Flow metadata store
feat(session): add PTY session identity
test(generation): boot candidate image in KVM
docs(architecture): document Stable and Evolution Worlds
```

Commits should represent coherent changes.

---

# 25. Issue labels

Create labels:

```text
type:feature
type:bug
type:architecture
type:research
type:docs
type:test
type:security

area:core
area:flow
area:session
area:goal
area:generation
area:evolution
area:memory
area:model
area:image
area:ui

priority:p0
priority:p1
priority:p2
priority:p3

status:blocked
status:needs-design
status:ready

agent-friendly
human-review
breaking
```

---

# 26. Milestones

Create GitHub milestones:

```text
0.0.1 Repository Foundation
0.0.2 Core Runtime Skeleton
0.0.3 Persistent State
0.0.4 Flows and Sessions
0.0.5 Generations
0.0.6 Evolution Lab
0.0.7 Goal Runtime
0.0.8 Agent Runtime
0.0.9 Minimal Agentic Environment
0.1.0 First Real Agentic OS
```

Do not create dozens of speculative milestones beyond 0.1 initially.

---

# 27. CI requirements

Initial GitHub Actions should include:

## Rust

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --release
```

## Go

When Go code exists:

```text
go test ./...
go vet ./...
```

## Python

When Python code exists:

```text
ruff
pytest
```

## Documentation

Check broken relative links, malformed Markdown where possible, and required docs.

## Security

Add when stable:

```text
cargo audit
cargo deny
```

Classify mandatory vs informational checks carefully.

---

# 28. Release model

Use semantic-ish pre-1.0 releases.

`0.0.x` means foundational internal capability.

`0.1.0` means first coherent real Agentic OS development release.

Every release should contain:

```text
source tag
release notes
build manifest
test result summary
known limitations
artifact checksums
```

Eventually also provide bootable images, packages, SBOM, signatures, and provenance metadata.

---

# 29. Sprint model

Use two-week sprints as the planning default.

Each sprint should contain:

```text
Goal
Deliverables
Acceptance criteria
Tests
Documentation
Risks
Exit condition
```

A sprint ends when its exit condition is satisfied or the work is explicitly re-planned.

---

# 30. Definition of Done

A core feature is complete only when:

- code implemented
- formatting/lint passes
- unit tests added where appropriate
- integration tests added where appropriate
- errors handled
- structured logs included
- relevant docs updated
- ADR added if architectural
- no known silent data corruption path
- CI passes
- user-visible behavior documented
- issue linked to commit/PR
- release notes updated if release-impacting

---

# 31. Sprint 0 — Repository Foundation

**Target release:** `0.0.1`

## Goal

Create a professional repository that can support long-term development by humans and agents.

## Deliverables

- initialize Git repository
- configure default branch `main`
- add MIT or Apache-2.0 license after project owner choice
- create Rust workspace
- create top-level structure
- create README
- create VISION
- create ARCHITECTURE
- create ROADMAP
- create CONTRIBUTING
- create SECURITY
- create DECISIONS
- create JOURNAL
- create ADR structure
- create `justfile`
- create initial GitHub Actions
- create issue templates
- create PR template
- create GitHub labels
- create milestones through 0.1.0
- add first architecture ADRs

## Initial ADRs

```text
ADR-0001 Rust is primary system language
ADR-0002 Go is preferred for distributed/orchestration services
ADR-0003 Python is research/prototyping language
ADR-0004 Upstream Linux remains kernel substrate
ADR-0005 SQLite is initial durable structured store
ADR-0006 systemd is initial service supervisor
ADR-0007 KVM is initial Evolution World virtualization layer
ADR-0008 Flows are first-class persistent OS objects
```

## Acceptance criteria

```text
just check
just test
```

succeed from a clean clone.

GitHub CI succeeds.

Repository documentation clearly explains the project.

---

# 32. Sprint 1 — Core Runtime Skeleton

**Target release:** `0.0.2`

## Goal

Establish the shared Rust foundations used by all services.

## Deliverables

Create:

```text
agentic-types
agentic-storage
agentic-ipc
agentic-core
```

Define strongly typed IDs:

```rust
FlowId
GoalId
AgentId
SessionId
GenerationId
ExperimentId
ExecutionId
```

Add structured config loading, tracing/journald logging, SQLite connection layer, migration framework, Unix socket IPC prototype, and basic daemon lifecycle helper.

Create minimal:

```text
agenticd
flowd
sessiond
goald
```

They may initially expose only health/status commands.

## Acceptance criteria

- all daemons start
- all daemons stop cleanly
- database migration runs
- IPC request/response works
- no daemon requires AI/network connectivity
- integration test starts services and queries status

---

# 33. Sprint 2 — Persistent State & Provenance

**Target release:** `0.0.3`

## Goal

Make Agentic OS capable of remembering durable system objects.

## Deliverables

Implement initial records:

```text
Flow
Goal
Session
Execution
Generation
ProvenanceEdge
```

Add migration-backed CRUD operations.

Create provenance relationships:

```text
created_by
caused_by
belongs_to
produced
tested_by
promoted_to
```

Implement an admin CLI:

```text
agenticctl
```

Initial commands:

```text
agenticctl status
agenticctl flow list
agenticctl goal list
agenticctl session list
agenticctl generation list
```

## Acceptance criteria

- state survives service restart
- migrations are tested
- object IDs remain stable
- provenance edges can be queried
- database corruption is not silently ignored

---

# 34. Sprint 3 — Flows & Persistent Sessions

**Target release:** `0.0.4`

## Goal

Implement the first genuinely distinctive Agentic OS primitive.

## Deliverables

Implement Flow:

```text
General Flow
create
rename
list
activate
archive
```

Initial states:

```text
FOREGROUND
ACTIVE
ARCHIVED
```

Implement persistent local PTY sessions.

Required behavior:

```text
create session
attach terminal
detach terminal
process continues
reattach later
terminate session
associate with Flow
```

CLI:

```text
agenticctl flow create Development
agenticctl flow activate Development
agenticctl session create --flow Development
agenticctl session list
agenticctl session attach <id>
```

## Acceptance criteria

A test must prove:

1. create Flow
2. start persistent shell
3. run long process
4. detach
5. switch Flow
6. reattach
7. process is still alive

---

# 35. Sprint 4 — System Generations

**Target release:** `0.0.5`

## Goal

Create a reproducible model for system state and rollback.

## Deliverables

Define Generation manifest.

Implement:

```text
generation create
generation list
generation inspect
generation activate
generation rollback
```

Initially the generation system may manage a constrained test root filesystem rather than the full installed distro.

Integrate source commit, package/build manifest, parent generation, reason, and provenance.

## Acceptance criteria

- generation manifest reproducible
- candidate can be created
- activation recorded
- rollback returns known previous state
- generation history survives reboot

---

# 36. Sprint 5 — Evolution World

**Target release:** `0.0.6`

## Goal

Automatically build and test candidate system generations inside KVM.

## Deliverables

Create:

```text
evolution/vm/
evolution/benchmarks/
evolution/promotion/
```

Automate:

```text
candidate build
      ↓
QEMU/KVM boot
      ↓
health test
      ↓
integration test
      ↓
shutdown
      ↓
result artifact
```

Add experiment record:

```text
Experiment {
    hypothesis
    candidate_generation
    baseline_generation
    test_plan
    metrics
    result
    decision
}
```

## Acceptance criteria

One command:

```text
just vm-test
```

builds a candidate, boots it, verifies a service, exits, and records success/failure.

---

# 37. Sprint 6 — Goal Runtime

**Target release:** `0.0.7`

## Goal

Introduce persistent goals as OS objects.

## Deliverables

Implement:

```text
Goal {
    id
    parent_goal
    objective
    rationale
    constraints
    success_metrics
    priority
    status
}
```

Commands:

```text
agenticctl goal create
agenticctl goal show
agenticctl goal child
agenticctl goal complete
agenticctl goal abandon
```

Add goal/provenance links to executions and generations.

## Acceptance criteria

A goal can produce a child goal, execution, experiment, and generation relationship.

---

# 38. Sprint 7 — Agent Runtime

**Target release:** `0.0.8`

## Goal

Allow bounded autonomous agents to operate the existing architecture.

## Deliverables

Implement initial agent model:

```text
Agent {
    id
    role
    goal
    capabilities
    status
}
```

Implement provider-neutral model interface.

Initial tools:

```text
read file
write workspace file
run command
query system state
query goals
create child goal
```

Do not begin with unrestricted autonomous root modification.

Every agent action should produce an Execution record.

## Acceptance criteria

Agent can receive goal, inspect a test project, modify source, run tests, record execution, and produce finding without losing provenance.

---

# 39. Sprint 8 — First Self-Development Loop

**Target release:** `0.0.9`

## Goal

Make Agentic OS capable of modifying a constrained part of itself through the normal development pipeline.

## Deliverables

Agent can:

```text
receive improvement goal
      ↓
create worktree
      ↓
modify source
      ↓
cargo check/test
      ↓
build candidate
      ↓
boot in Evolution World
      ↓
run tests
      ↓
record evidence
```

Promotion may still require human approval.

## Acceptance criteria

Demonstrate one real change made by an agent to Agentic OS source, successfully tested in KVM and represented through provenance.

---

# 40. Sprint 9 — Minimal Distro Image

**Target release:** included before `0.1.0`

## Goal

Move from service architecture to a bootable Agentic OS image.

## Deliverables

- reproducible root filesystem
- upstream Linux package
- initramfs
- systemd
- networking
- users
- SSH or console access
- Agentic OS services installed
- `agenticctl`
- General Flow exists
- logs available through journald
- recovery path documented

Prefer VM-first installation.

Do not optimize for broad hardware support yet.

## Acceptance criteria

A clean build produces a bootable KVM image from source.

---

# 41. Sprint 10 — Release `0.1.0`

## Goal

Produce the first coherent real Agentic OS development release.

`0.1.0` should demonstrate:

- real Linux boot
- Agentic OS services
- persistent state
- General Flow
- persistent terminal sessions
- goals
- provenance
- generations
- rollback
- Evolution World
- provider-neutral agent runtime
- one bounded self-development demonstration

It does not need polished graphical shell, ambient voice, advanced user model, gaming Flow, art Flow, broad hardware installer, or cross-device Flow sync.

---

# 42. Post-0.1 roadmap

## 0.2 — Flow OS

- richer Flow lifecycle
- quiescent/hibernated states
- nested Flows
- Flow revisions
- resource policy
- cgroup integration
- Flow-aware notifications
- Flow-aware credentials
- Flow templates
- intuitive Flow creation

## 0.3 — Adaptive Agentic Shell

- initial Wayland shell
- Flow switcher
- project-centric UI
- terminal/session views
- agent activity view
- goal view
- system history
- adaptive layout

## 0.4 — Ambient Interaction

- local VAD
- wake/attention detection
- local speech-to-text
- text/voice intent normalization
- conversational context
- clear microphone-state model

## 0.5 — User Model

- explicit facts
- observed behavior
- inferred preferences
- confidence
- adaptation proposals
- reversible UX evolution

## 0.6 — Autonomous Evolution

- automatic hypothesis generation
- candidate experiments
- multidimensional fitness
- safe promotion policies
- automatic rejection
- retained failure knowledge

## 0.7 — Multi-machine Agentic OS

- Flow synchronization
- remote execution
- server Flows
- cluster control
- distributed agents
- machine-specific vs user-specific state separation

## 1.0 — Daily-drivable self-evolving Agentic OS

Exact scope should be determined from evidence accumulated before then.

---

# 43. Release gates

A release cannot be tagged if:

- CI is red
- database migration path is broken
- boot test fails
- rollback test fails for generation-related releases
- known critical data corruption exists
- release notes are missing

Pre-1.0 releases may contain incomplete features, but failures must be explicit.

---

# 44. Testing strategy

Use unit tests for IDs, state transitions, policy evaluation, serialization, goal graph, provenance graph.

Use integration tests for IPC, SQLite, Flow lifecycle, session attach/detach, and generation management.

Use KVM boot tests to verify systemd target, Agentic services, storage, and networking.

Use Evolution tests for candidate generation and promotion/rejection.

Use recovery tests by intentionally breaking components and verifying rollback/recovery.

---

# 45. Security direction

Create `THREAT_MODEL.md` early.

Initial categories:

- compromised model provider
- malicious tool output
- prompt injection from external content
- privilege escalation
- poisoned package
- corrupted generation
- credential leakage
- agent confused-deputy behavior
- persistent malicious memory
- unsafe Flow credential crossover

Do not attempt to solve every security problem during Sprint 0. Document them and evolve mitigations systematically.

---

# 46. Provenance requirements

Every meaningful autonomous action should eventually answer:

```text
WHO?
WHY?
WHICH GOAL?
WHICH AGENT?
WHICH MODEL?
WHICH COMMAND?
WHICH SOURCE?
WHICH TEST?
WHICH GENERATION?
```

Example:

```text
Execution E-992
├── agent A-12
├── goal G-481
├── command cargo test
├── result success
├── artifact test-report-82
└── candidate generation 000019
```

---

# 47. OpenCode operating rules

When OpenCode works on this repository:

1. Read `VISION.md`.
2. Read `ARCHITECTURE.md`.
3. Read relevant ADRs.
4. Read current milestone and sprint section in `ROADMAP.md`.
5. Inspect existing code before designing replacements.
6. Use existing Linux primitives where possible.
7. Do not invent major infrastructure without evidence.
8. Keep changes scoped to the current milestone.
9. Add tests before claiming success.
10. Run all relevant checks.
11. Update documentation when architecture changes.
12. Add ADR when making consequential architectural decisions.
13. Do not silently change language policy.
14. Do not introduce a kernel fork.
15. Do not couple the system to one AI provider.
16. Do not make boot depend on AI availability.
17. Do not bypass provenance for autonomous actions.
18. Do not merge experimental code into the trusted core without tests.
19. Prefer simple designs over speculative abstractions.
20. Leave the repository buildable after every completed task.

---

# 48. OpenCode task execution format

For each issue, OpenCode should work in this format.

## Before coding

Report:

```text
Task:
Current architecture:
Files likely affected:
Design:
Risks:
Tests planned:
```

If an architectural contradiction is found, document it and propose an ADR or issue.

## Before finishing

Run:

```text
just check
just test
```

plus task-specific integration tests.

Report:

```text
Implemented:
Tests:
Architecture changes:
Docs updated:
Known limitations:
Next recommended issue:
```

---

# 49. Initial GitHub issue set

Create issues for at least:

```text
#1 Bootstrap Rust workspace
#2 Add CI
#3 Add architecture documentation
#4 Add ADR framework
#5 Implement typed core IDs
#6 Add SQLite storage layer
#7 Add migration framework
#8 Implement structured logging
#9 Implement basic Unix socket IPC
#10 Create agenticctl skeleton
#11 Implement Flow data model
#12 Create General Flow
#13 Implement Flow persistence
#14 Implement PTY session service
#15 Add detach/reattach integration test
#16 Implement provenance graph
#17 Implement generation manifest
#18 Build candidate root filesystem
#19 Add KVM boot harness
#20 Add first automated VM health test
#21 Implement Goal data model
#22 Implement provider-neutral model interface
#23 Implement Execution records
#24 Implement bounded agent runtime
#25 Demonstrate first self-development experiment
```

Assign these to milestones according to the sprint roadmap.

---

# 50. Repository README outline

OpenCode should create a README with:

```text
Agentic OS
What it is
Why it exists
Current status
Architecture overview
Core concepts
Build prerequisites
Quick start
Running tests
Running VM
Repository layout
Roadmap
Contributing
Security status
License
```

Do not market unfinished capabilities as already implemented.

Clearly distinguish:

```text
Implemented
In progress
Planned
Research
```

---

# 51. Development environment

Initial supported developer host:

```text
Linux x86-64
```

Recommended dependencies:

- Rust toolchain
- Cargo
- Go
- Python 3
- QEMU
- KVM
- systemd development libraries where needed
- SQLite
- Git
- just
- common build essentials

Avoid requiring Docker if native tooling is simpler.

---

# 52. First technical vertical slice

```text
boot normal Linux development host
      ↓
run flowd + sessiond
      ↓
General Flow exists
      ↓
create Development Flow
      ↓
create PTY session
      ↓
run long process
      ↓
detach
      ↓
activate General
      ↓
activate Development
      ↓
reattach
      ↓
process still alive
```

This demonstrates a real Agentic OS concept without requiring the entire distro image first.

---

# 53. Second technical vertical slice

```text
source change
      ↓
candidate generation
      ↓
KVM boot
      ↓
health checks
      ↓
test result
      ↓
promotion record
```

This demonstrates the self-evolution foundation.

---

# 54. Third technical vertical slice

```text
Goal
  ↓
Agent
  ↓
source inspection
  ↓
change
  ↓
test
  ↓
candidate generation
  ↓
KVM
  ↓
evidence
```

This demonstrates the beginning of autonomous self-development.

---

# 55. Things OpenCode must not do early

Do not:

- fork the Linux kernel
- invent a new package format
- invent a new init system
- replace systemd
- build a custom container runtime
- build a custom VM hypervisor
- invent a database
- create a blockchain
- implement a massive microservice architecture
- make every component networked
- build a polished desktop before core primitives exist
- implement always-on voice before privacy and context architecture exists
- add dozens of model providers in Sprint 0
- create a generic plugin system before there are real plugin needs
- create complex distributed consensus before multi-machine operation exists
- optimize prematurely
- create abstractions solely because they sound future-proof

Use boring Linux technology wherever boring technology works.

---

# 56. Things OpenCode should optimize for

Prioritize:

```text
correctness
inspectability
reproducibility
reversibility
testability
clear ownership
stable IDs
good errors
good logs
small coherent modules
easy local development
easy KVM testing
```

Performance matters, but architecture comes first until real bottlenecks exist.

---

# 57. Initial architecture decisions to preserve

Unless explicitly changed through ADR:

```text
Rust is the default system language.
Go is for distributed/orchestration-heavy services.
Python is for experimentation and analysis.
C is used only where low-level integration justifies it.
Java is ecosystem-specific.

Linux remains upstream.
systemd remains service supervisor.
SQLite is initial structured storage.
KVM is the Evolution World.
Flows are first-class.
Goals are persistent.
Provenance is mandatory for autonomous system changes.
Generations are reversible.
AI providers are replaceable.
The system works without AI connectivity.
```

---

# 58. Copy-paste OpenCode bootstrap prompt

## BEGIN OPENCODE PROMPT

You are bootstrapping the GitHub repository for **Agentic OS**, a self-evolving Linux distribution.

Your job is not to create a toy AI assistant. Build the foundation of a real long-lived operating-system project.

Read this entire specification before changing files.

### Mission

Agentic OS is a Linux distribution in which autonomous agents are first-class system actors. It should eventually understand user intent, maintain persistent goals and memory, create persistent activity contexts called Flows, operate Linux, develop software, build candidate system generations, test them in QEMU/KVM, evaluate evidence, promote successful changes, reject failed changes, and evolve the userspace around the owner's actual needs.

Linux remains the stable upstream substrate.

Do not fork the Linux kernel.

### Primary language policy

Use:

- Rust for trusted long-lived OS services and core runtime.
- Go for distributed systems, networking-heavy orchestration, synchronization, and remote services.
- Python for AI/ML research, data analysis, experiments, evaluation, migrations, and temporary tools.
- C only when direct low-level Linux integration clearly benefits from it.
- Java only when a JVM ecosystem provides a concrete advantage.

Do not attempt to rewrite existing Linux infrastructure.

### Core principles

1. The OS must boot and remain administrable without an AI provider.
2. Use existing Linux primitives.
3. Persistent goals are first-class objects.
4. Flows are first-class persistent activity contexts.
5. Terminal sessions are persistent system objects, not merely windows.
6. System changes should become reversible generations.
7. Aggressive experiments happen in a QEMU/KVM Evolution World.
8. Every meaningful autonomous change must have provenance.
9. Humans and agents use the same Git/build/test/release pipeline.
10. AI providers must be replaceable.

### Repository task

Create a professional repository foundation with:

- Rust Cargo workspace
- repository layout from this specification
- README.md
- VISION.md
- ARCHITECTURE.md
- ROADMAP.md
- CONTRIBUTING.md
- SECURITY.md
- DECISIONS.md
- JOURNAL.md
- docs/
- adr/
- rfcs/
- tests/
- image/
- evolution/
- packaging/
- kernel/
- Python/Go/C areas as documented
- justfile
- GitHub Actions
- issue templates
- PR template

Do not create meaningless empty scaffolding. Directories that do not yet contain implementation should have a short README explaining their intended role.

### Initial Rust workspace

Create reusable crates:

- agentic-types
- agentic-storage
- agentic-ipc
- agentic-core

Create minimal services:

- agenticd
- flowd
- sessiond
- goald

Each daemon should start cleanly, support structured tracing, exit cleanly, have tests where useful, and not require network connectivity or an AI provider.

### Typed IDs

Introduce strongly typed IDs for at least:

- FlowId
- GoalId
- AgentId
- SessionId
- GenerationId
- ExperimentId
- ExecutionId

Do not pass raw strings everywhere.

### Persistent state

Use SQLite.

Add a migration mechanism from the start.

Do not manually mutate schema outside migrations.

### IPC

Start with simple local Unix domain socket IPC.

Do not design a distributed protocol yet.

### Developer commands

Provide:

```text
just check
just test
just lint
just build
```

Add placeholders only when clearly marked for later commands:

```text
just image
just vm
just vm-test
```

Do not pretend unimplemented commands work.

### CI

Add GitHub Actions for:

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --release
```

Add Go/Python checks only if actual Go/Python code is present.

### Documentation

Create initial ADRs:

- Rust as primary system language
- Go for distributed/orchestration services
- Python for experimentation
- upstream Linux kernel policy
- SQLite initial state store
- systemd service supervision
- KVM Evolution World
- Flows as first-class OS objects

### GitHub planning

Create or provide scripts/instructions to create these milestones:

- 0.0.1 Repository Foundation
- 0.0.2 Core Runtime Skeleton
- 0.0.3 Persistent State
- 0.0.4 Flows and Sessions
- 0.0.5 Generations
- 0.0.6 Evolution Lab
- 0.0.7 Goal Runtime
- 0.0.8 Agent Runtime
- 0.0.9 Minimal Agentic Environment
- 0.1.0 First Real Agentic OS

Create issue definitions for the initial roadmap.

If GitHub credentials or CLI access are available, create issues/milestones directly.

If not available, create a deterministic script or markdown checklist that the owner can execute later.

Do not block repository bootstrap merely because GitHub API access is unavailable.

### Sprint execution

First implement only **Sprint 0: Repository Foundation**.

Do not jump ahead and build the whole OS.

At completion, report:

```text
Repository structure created:
Architecture decisions:
CI:
Tests:
Commands:
GitHub planning created:
Known limitations:
Recommended first issue for Sprint 1:
```

Run all relevant checks before declaring success.

The repository must remain buildable.

Prefer simple, boring, inspectable architecture over speculative complexity.

## END OPENCODE PROMPT

---

# 59. Follow-up OpenCode prompt for Sprint 1

```text
Implement Sprint 1 — Core Runtime Skeleton from ROADMAP.md.

Read VISION.md, ARCHITECTURE.md, all accepted ADRs, and existing code first.

Do not redesign the project unless the existing architecture creates a concrete blocker.

Deliver:

- agentic-types
- agentic-storage
- agentic-ipc
- agentic-core
- minimal agenticd
- minimal flowd
- minimal sessiond
- minimal goald
- tracing
- SQLite migrations
- Unix socket IPC prototype
- integration test proving services can start and answer health/status

Run:

just check
just test

Update JOURNAL.md and relevant architecture docs.

Do not implement Sprint 2 work unless required for a minimal coherent interface.
```

---

# 60. Follow-up OpenCode prompt for Sprint 3 — Flows

```text
Implement the Flow and persistent-session vertical slice.

Required result:

1. General Flow exists automatically.
2. User can create Development Flow.
3. User can activate a Flow.
4. User can create a persistent PTY shell assigned to a Flow.
5. A long-running process remains alive after terminal presentation detaches.
6. User can activate another Flow.
7. User can return and reattach to the original PTY.
8. Session and Flow state survive service restart where architecturally appropriate.
9. Integration tests prove the behavior.
10. All operations produce structured logs and durable IDs.

Do not build a graphical shell yet.

CLI is sufficient.

Use Linux PTYs and existing process primitives rather than inventing a pseudo-terminal abstraction from scratch.
```

---

# 61. Follow-up OpenCode prompt for Evolution World

```text
Implement the first Evolution World vertical slice.

Required flow:

source tree
  ↓
candidate build
  ↓
candidate generation manifest
  ↓
QEMU/KVM boot
  ↓
health check
  ↓
integration test
  ↓
shutdown
  ↓
test artifact
  ↓
experiment result

Expose the workflow through:

just vm-test

Do not implement automatic production promotion yet.

Record enough provenance that a test result can be traced to:

- source commit
- generation
- experiment
- build
- VM run
- test output
```

---

# 62. Success criteria for the repository bootstrap

The repository bootstrap is successful when a new developer or agent can:

```text
git clone ...
cd agentic-os
just check
just test
```

and then understand:

- what Agentic OS is
- what is implemented today
- what is planned
- which languages to use
- how architecture decisions are made
- what Sprint 1 contains
- how releases are structured
- how to contribute without inventing a parallel architecture

---

# 63. The central engineering rule

Agentic OS is ambitious.

The implementation strategy must therefore be conservative.

Build one real primitive at a time.

```text
persistent state
    ↓
persistent Flow
    ↓
persistent session
    ↓
generation
    ↓
Evolution World
    ↓
goal
    ↓
agent
    ↓
self-development
    ↓
adaptive interaction
    ↓
autonomous evolution
```

Every stage should be useful, testable, inspectable, and capable of becoming part of the final system.

---

# 64. Final target

The repository should gradually become the source of a system capable of this:

```text
Human:
    "Improve yourself."

Agentic OS:
    understands current system
        ↓
    creates improvement goal
        ↓
    inspects its own source
        ↓
    creates worktree
        ↓
    modifies source
        ↓
    runs tests
        ↓
    builds candidate generation
        ↓
    boots candidate in Evolution World
        ↓
    benchmarks it
        ↓
    compares evidence
        ↓
    rejects or promotes candidate
        ↓
    records provenance
        ↓
    continues operating
```

At the same time, for an ordinary user, the same operating system may simply feel like:

```text
"Go to my art flow."
"Continue the thing I was drawing."
"Can you help me animate this?"
"Back to work."
"Show the server dashboard."
```

The sophistication belongs underneath the experience.

That is the engineering direction for Agentic OS.
