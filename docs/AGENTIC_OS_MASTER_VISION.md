# Agentic OS

> **A self-evolving Linux distribution designed around autonomous AI
> agents as first-class operators, developers, and users of the
> operating system.**

**Status:** Foundational architecture and vision\
**Scope:** A real Linux distribution, not an AI assistant layered on top
of an existing desktop\
**Kernel strategy:** Upstream Linux remains the stable substrate and
updates normally with the Linux ecosystem\
**Primary idea:** The operating system adapts, transforms, simplifies,
and evolves around the actual needs and behavior of its owner

------------------------------------------------------------------------

## 1. Vision

Agentic OS is a Linux distribution whose defining property is that the
operating environment itself can evolve.

It is not simply Linux with a chatbot, a Copilot window, or an LLM
connected to shell commands. Autonomous agents are part of the operating
system's architecture. They can observe the system, understand goals,
operate Linux, develop software, create experiments, test candidate
changes, maintain persistent knowledge, and evolve the userspace over
time.

The initial installation is therefore a **seed OS**.

Two people may install the same seed image and, years later, have
operating systems that look and behave radically differently because
their machines have adapted to different people, hardware, workflows,
preferences, accessibility requirements, projects, and interests.

The long-term objective is:

> **Agentic OS must be capable of becoming something its original
> developers did not design.**

The system should not merely accumulate configuration. It should be
capable of changing its abstractions, interfaces, workflows, services,
agents, applications, automation, memory architecture, and eventually
large portions of its own userspace.

------------------------------------------------------------------------

## 2. Core philosophy

Agentic OS is built around several principles.

### 2.1 Linux is the substrate

Linux already provides an extraordinarily flexible kernel and userspace
ecosystem.

Agentic OS does not begin by modifying the kernel. Kernel development
continues upstream, and Agentic OS follows stable kernel releases and
security updates like a normal distribution.

The innovation initially happens above the kernel.

``` text
hardware
   ↑
Linux kernel
   ↑
Agentic OS distribution
   ↑
agents
   ↑
user
```

The wrong model is:

``` text
Linux
   ↑
tiny restricted AI sandbox
```

Agents should be able to use everything that ordinary Linux software and
administrators can use according to their authority.

This includes:

-   shell commands
-   syscalls
-   `/proc`
-   `/sys`
-   devices and ioctls
-   filesystems and mounts
-   sockets
-   signals and pidfds
-   netlink
-   cgroups v2
-   namespaces
-   seccomp
-   KVM
-   eBPF where useful
-   systemd
-   D-Bus
-   journald
-   networking
-   package tools
-   containers
-   compilers
-   Git
-   ordinary applications

Structured Agentic APIs are conveniences and provenance boundaries, not
replacements for Linux.

### 2.2 The OS adapts to the human

Traditional operating systems expect humans to adapt themselves to a
mostly static computer interface.

Agentic OS reverses part of that relationship.

The computer observes how it is actually used and can gradually adapt
its environment.

A child interested in unicorns and drawing might initially develop a
playful, highly visual environment organized around creation. Years
later, if that same person becomes interested in programming, the OS may
gradually expose terminals, source control, development environments,
automation, debugging tools, and increasingly advanced concepts.

The important part is not the unicorn theme.

The important part is that the **structure of the operating environment
can change as the user's needs change**.

### 2.3 Complexity is earned by use

The system should not present every possible advanced feature from the
beginning.

At the same time, it should not permanently hide power from the user.

Complexity should appear when it becomes useful.

``` text
simple needs
    ↓
simple environment
    ↓
new behavior / interests
    ↓
new capabilities become useful
    ↓
environment evolves
    ↓
advanced environment
```

The same installation can therefore grow with its owner.

### 2.4 Adaptation is not engagement optimization

The objective is not to maximize screen time, clicks, retention,
notifications, or emotional dependency.

The system should optimize for actual usefulness:

-   accomplishing goals
-   reducing unnecessary work
-   making tools understandable
-   improving accessibility
-   reducing friction
-   improving reliability
-   simplifying repeated workflows
-   helping the user create and learn
-   preserving user control

Especially for children, personalization must not become behavioral
manipulation.

------------------------------------------------------------------------

## 3. What Agentic OS is

Agentic OS can be described as several systems combined:

1.  A real Linux distribution.
2.  An autonomous agent runtime.
3.  A persistent goal system.
4.  A system observation layer.
5.  A durable memory and knowledge architecture.
6.  A user model.
7.  A software development environment for its own agents.
8.  A generational operating-system build system.
9.  An experimentation and evolution engine.
10. An adaptive human-computer interface.

Together these form an operating system capable of deliberate
transformation.

------------------------------------------------------------------------

## 4. High-level architecture

``` text
                         USER
                   voice / text / GUI
                          │
                          ▼
                   ┌─────────────┐
                   │ Intent Core │
                   └──────┬──────┘
                          │
                          ▼
                      Goal Graph
                          │
               ┌──────────┼──────────┐
               ▼          ▼          ▼
            Agents     Planner    Evolution
               │          │          │
               └──────────┼──────────┘
                          ▼
                 Identity / Policy /
                    Provenance
                          │
        ┌─────────────────┼─────────────────┐
        ▼                 ▼                 ▼
      Files             Apps             System
      Git               Web              Services
      Projects          GUI              Hardware
        │                 │                 │
        └─────────────────┼─────────────────┘
                          ▼
                    Linux userspace
                          │
                          ▼
                     Linux kernel
                          │
                          ▼
                       Hardware
```

Another view:

``` text
┌─────────────────────────────────────────────┐
│                    HUMAN                    │
├─────────────────────────────────────────────┤
│ Adaptive Experience                         │ ← evolves
├─────────────────────────────────────────────┤
│ Agents / Goals / Planning                   │ ← evolves
├─────────────────────────────────────────────┤
│ Memory / Knowledge / User Model             │ ← evolves
├─────────────────────────────────────────────┤
│ Evolution / Experiments / Fitness           │ ← evolves
├─────────────────────────────────────────────┤
│ Capabilities / Security / Provenance        │ ← evolves carefully
├─────────────────────────────────────────────┤
│ Services / Packages / Environments          │ ← evolves
├─────────────────────────────────────────────┤
│ Agentic Shell / Wayland / Applications      │ ← evolves
├─────────────────────────────────────────────┤
│ Linux userspace integration                 │ ← evolves
╞═════════════════════════════════════════════╡
│               UPSTREAM LINUX                │
│              STABLE SUBSTRATE               │
└─────────────────────────────────────────────┘
```

------------------------------------------------------------------------

## 5. A real Linux distribution

Agentic OS should be engineered as a real distribution from the
beginning.

It must eventually support the ordinary responsibilities of a Linux
distro:

-   reproducible system images
-   UEFI boot
-   initramfs
-   kernel packaging
-   firmware
-   hardware discovery
-   networking
-   users and permissions
-   storage
-   filesystem management
-   encryption
-   locales and time
-   service management
-   logging
-   package dependencies
-   repositories
-   package signing
-   updates
-   recovery
-   rollback
-   desktop stack
-   security updates
-   kernel updates
-   firmware updates
-   installation on virtual machines
-   installation on physical hardware
-   automated boot tests
-   upgrade tests
-   integration tests

A critical property is:

> **Agentic OS must still function as Linux when the model provider is
> unavailable.**

Boot, login, networking, recovery, system updates, administration, and
access to user data cannot depend on a successful LLM request.

------------------------------------------------------------------------

## 6. Upstream Linux policy

Linux is treated as an upstream dependency.

The kernel is allowed to update normally as Linux development
progresses.

Agentic OS may maintain:

-   kernel configuration
-   packaging
-   boot integration
-   hardware policies
-   tested kernel versions

but should initially avoid carrying custom kernel patches unless a
concrete requirement appears.

A future agent may eventually discover that userspace cannot support an
important capability efficiently. That evidence could justify kernel
research later.

That is deliberately outside the initial architecture.

------------------------------------------------------------------------

## 7. Agents as first-class system actors

An Agentic OS agent is not merely a chatbot with a list of approved
tools.

It is a software actor operating inside Linux.

Depending on its role and authority, an agent may:

-   inspect processes
-   inspect logs
-   start and stop services
-   edit files
-   compile software
-   install packages
-   create containers
-   create virtual machines
-   inspect network state
-   use Git
-   run tests
-   benchmark software
-   inspect hardware
-   modify configuration
-   develop new system services
-   build a new Agentic OS generation

Agents may use three layers.

### Layer 1 --- structured Agentic APIs

``` text
services.start("postgresql")
services.restart("agent-indexer")

processes.list()
processes.signal(pid, SIGTERM)

system.cpu()
system.memory()
system.disk()

logs.query(...)
logs.follow("agent-indexer")

generation.build(...)
goal.create(...)
experiment.run(...)
```

These APIs improve typing, provenance, policy, and repeated automation.

### Layer 2 --- ordinary Linux tools

``` text
systemctl
journalctl
ip
ss
mount
lsblk
git
cargo
gcc
python
podman
qemu
```

### Layer 3 --- direct Linux interfaces

Software written by agents may directly use:

``` text
/proc
/sys
netlink
cgroups
namespaces
sockets
signals
pidfds
KVM
eBPF
ioctls
filesystem APIs
```

The operating system should not require a new `agentic_do_thing()` API
before agents can use a Linux capability.

------------------------------------------------------------------------

## 8. Agentic Core

The core runtime can initially be implemented in fewer processes, but
its conceptual domains should remain separated.

Possible components:

``` text
agenticd
    orchestration
    agent lifecycle
    execution

goald
    goals
    dependencies
    priorities
    success criteria

observed
    system observations
    metrics
    events

memoryd
    durable knowledge
    retrieval
    user/system memory

evolutiond
    experiments
    candidate generations
    evaluation
    promotion

capabilityd
    identity
    authority
    delegation
    policy

provenanced
    causal history
    audit

modeld
    model-provider abstraction
```

This prevents the architecture from becoming one enormous AI daemon that
controls everything.

------------------------------------------------------------------------

## 9. Intent

Human interaction may arrive through many interfaces:

``` text
keyboard ─┐
voice ────┼──► Intent API ─► Agent Runtime
GUI ──────┘
```

The input mechanism is not the fundamental concept.

The fundamental concept is **intent**.

Examples:

``` text
"Make this computer easier for my daughter to use."

"Help me finish this programming project."

"My builds are getting slow."

"Organize my photos."

"Make yourself better."

"I don't want notifications while I work."

"Why did you install this service?"
```

Intent becomes goals, constraints, preferences, or questions depending
on context.

------------------------------------------------------------------------

## 10. Goals as operating-system objects

Goals should be persistent objects rather than temporary chat messages.

Example schema:

``` text
Goal {
    id
    parent_goal
    created_by
    objective
    rationale
    constraints
    permissions
    success_metrics
    evidence
    priority
    estimated_cost
    status
}
```

Example graph:

``` text
G-100 Improve owner's development environment

├── G-101 Reduce build latency
│   ├── G-104 Profile compiler
│   └── G-105 Test build cache
│
├── G-102 Improve project navigation
│
└── G-103 Reduce interruptions
```

Agents may:

-   create child goals
-   suspend goals
-   abandon failed goals
-   create experiments
-   attach evidence
-   discover new goals
-   revise plans
-   request human decisions where appropriate

The human primarily supplies values, desires, boundaries, and
corrections.

The system can increasingly determine intermediate work itself.

------------------------------------------------------------------------

## 11. Multi-agent architecture

Not every problem requires many agents.

Agent specialization becomes useful when different responsibilities
benefit from different context, tools, models, or perspectives.

Possible roles:

-   Planner
-   Researcher
-   Builder
-   Reviewer
-   Benchmark
-   Security
-   Maintainer
-   UX
-   Evolution

Temporary specialized agents can be created for individual goals.

A child agent receives relevant goal context and explicitly delegated
authority.

A useful principle is:

> **Goals may be delegated downward. Privileges do not automatically
> follow them.**

### Structured collaboration

Agents should communicate primarily through durable artifacts rather
than endless natural-language conversations.

Example:

``` text
Finding F-829

producer:
    performance-agent

claim:
    build latency is dominated by dependency scanning

evidence:
    benchmark-441
    trace-882

confidence:
    high

affected_goals:
    G-102
    G-103
```

Agents are allowed to disagree.

The system should preserve:

-   arguments
-   evidence
-   measurements
-   assumptions
-   rejected alternatives

rather than reducing disagreement to a vote.

------------------------------------------------------------------------

## 12. Persistent memory

The model's context window is not the operating system's memory.

Persistent state belongs to Agentic OS.

``` text
SQLite / object store / Git / indexes
              │
              ▼
         Memory service
              │
        relevant retrieval
              │
              ▼
             Model
```

Persistent state may include:

-   goals
-   observations
-   decisions
-   findings
-   user preferences
-   user corrections
-   accepted changes
-   rejected changes
-   system history
-   experiments
-   benchmarks
-   project knowledge
-   workflows
-   agent-created documentation

The model provider can therefore change without erasing the system's
identity.

------------------------------------------------------------------------

## 13. User model

Adaptation requires a model of the user, but the system must preserve
the distinction between fact and inference.

Three important categories are:

### Explicit

The user directly stated something.

``` text
"I like unicorns."
```

### Observed

The system observed behavior.

``` text
The user repeatedly chooses unicorn imagery.
```

### Inferred

The system forms a hypothesis.

``` text
The user may prefer a playful visual style.
confidence: 0.84
```

An inference must never silently become an explicit fact.

Possible structure:

``` text
UserModel/
├── explicit/
├── observed/
├── inferred/
├── workflows/
├── preferences/
├── accessibility/
├── accepted_changes/
└── rejected_changes/
```

Every inference should carry provenance and confidence.

------------------------------------------------------------------------

## 14. Adaptive experience

The desktop is not necessarily a fixed arrangement of applications and
windows forever.

Traditional applications can continue running under Wayland, but Agentic
OS can introduce higher-level concepts such as:

-   Intent
-   Goals
-   Projects
-   Tasks
-   Experiments
-   Artifacts
-   Decisions
-   History

Example shell:

``` text
┌───────────────────────────────────────────────────┐
│ Agentic OS                              Gen 00184 │
├───────────────────────────────────────────────────┤
│ What do you want?                                 │
│ > _____________________________________________   │
│                                                   │
│ Active                                            │
│ ● Improve development workflow                    │
│    Researching build latency                      │
│                                                   │
│ Autonomous                                        │
│ ● Testing candidate generation                    │
│ ● Simplifying project indexing service            │
├───────────────────────────────────────────────────┤
│ Goals  Projects  History  System  Experiments     │
└───────────────────────────────────────────────────┘
```

This UI itself may evolve.

------------------------------------------------------------------------

## 15. Example: an evolving child's computer

Imagine a young user.

Initially the system observes that they:

-   like unicorns
-   draw frequently
-   use only a few applications
-   prefer visual interaction

The system might gradually create:

-   a playful visual theme
-   larger creative controls
-   simplified navigation
-   drawing-oriented workflows
-   easy access to recent creations

Later the user begins experimenting with programming.

The environment might gradually expose:

-   scripts
-   automation
-   source files
-   a terminal
-   Git
-   programming concepts
-   development tools

Years later the same installation might resemble a sophisticated
engineering workstation.

The important property is continuity.

The computer grows with the human instead of requiring the human to
periodically abandon one environment and learn another from scratch.

------------------------------------------------------------------------

## 16. Three scopes of evolution

Not every discovered improvement belongs in the same place.

### Distribution evolution

General improvements appropriate for Agentic OS itself.

Example:

``` text
A better generic service-diagnostic subsystem.
```

### Machine evolution

Adaptation to one computer.

Example:

``` text
Resource policy optimized for this CPU, GPU, RAM and workload.
```

### User evolution

Adaptation to one person.

Example:

``` text
Interface organization, workflows, accessibility and visual style.
```

This distinction prevents personal preferences from accidentally
becoming global distro behavior.

A personal discovery may nevertheless lead to a general mechanism.

``` text
PERSONAL DISCOVERY
        ↓
GENERALIZATION
        ↓
EXPERIMENT
        ↓
UPSTREAM IMPROVEMENT
```

------------------------------------------------------------------------

## 17. Evolution engine

The central loop is:

``` text
OBSERVE
   ↓
FIND PATTERN
   ↓
FORM HYPOTHESIS
   ↓
CREATE GOAL
   ↓
DESIGN EXPERIMENT
   ↓
BUILD CANDIDATE
   ↓
TEST
   ↓
MEASURE
   ↓
REVIEW
   ↓
ACCEPT / REJECT
   ↓
NEW GENERATION
   ↓
OBSERVE
```

Example:

``` text
Observation:
    The user repeatedly performs the same build/deploy sequence.

Hypothesis:
    This workflow can be automated.

Goal:
    Reduce repetitive deployment work.

Experiment:
    Build a deployment abstraction.

Candidate:
    New service + shell integration.

Test:
    Replay workflow in Evolution World.

Measurement:
    14 manual steps -> 2
    no reliability regression

Decision:
    Accept.

Generation:
    185
```

The OS has now evolved because of actual use.

------------------------------------------------------------------------

## 18. Evolution must also remove things

A self-evolving OS that only adds features will eventually become
unusable.

Evolution therefore includes deletion.

The system should continuously be able to ask:

> **What can I remove?**

Candidates may include:

-   unused services
-   obsolete workflows
-   redundant agents
-   old compatibility layers
-   abandoned UI concepts
-   duplicate automation
-   unnecessary dependencies
-   configuration that no longer serves a purpose

Removal should be tested against regression suites just like additions.

------------------------------------------------------------------------

## 19. Fitness and selection

No single metric should determine whether a change is good.

Especially not engagement.

Fitness is multidimensional.

Possible dimensions:

-   utility
-   task completion
-   latency
-   performance
-   reliability
-   resource usage
-   user friction
-   complexity
-   maintainability
-   accessibility
-   reversibility
-   security
-   explicit user satisfaction

Example:

``` text
Candidate 185

performance:       +18%
memory:             +2%
complexity:        +35%
reliability:        -3%
user benefit:       +1%

Decision:
    REJECT

Reason:
    The performance improvement does not justify the
    maintenance and reliability cost.
```

This prevents the evolution engine from blindly maximizing one number.

------------------------------------------------------------------------

## 20. Stable World and Evolution World

Agentic OS should separate the computer currently relied upon by the
user from the environment where aggressive experiments occur.

``` text
             PHYSICAL COMPUTER

┌────────────────────────────────────┐
│            STABLE WORLD            │
│                                    │
│ User's actual Agentic OS           │
│ Generation N                       │
└────────────────┬───────────────────┘
                 │ snapshot / source
                 ▼
┌────────────────────────────────────┐
│          EVOLUTION WORLD           │
│                                    │
│ QEMU/KVM                           │
│ containers / namespaces            │
│ candidate N+1a                     │
│ candidate N+1b                     │
│ candidate N+1c                     │
└────────────────┬───────────────────┘
                 │ evidence
                 ▼
              promotion
                 │
                 ▼
           Generation N+1
```

Inside Evolution World, agents can have extremely broad autonomy.

They may:

-   install packages
-   delete packages
-   break services
-   rewrite configuration
-   build new daemons
-   restart the system
-   alter the desktop
-   experiment with dependencies
-   deliberately test failure modes

The consequence is contained.

This is a much more useful boundary than arbitrarily forbidding powerful
operations.

------------------------------------------------------------------------

## 21. Generational operating system

Agentic OS should represent system state as generations.

Example:

``` text
Generation 000184

kernel:
    upstream Linux package version

system:
    hash: ...

agent-runtime:
    hash: ...

configuration:
    hash: ...

parent:
    generation-000183

reason:
    goal-182

experiment:
    exp-993

status:
    stable
```

Changes create candidate generations.

``` text
CURRENT
   │
   ├──────────────► snapshot
   ▼
candidate generation
   │
   ▼
tests
   ├── bad ──► discard
   │
   └── good ─► activate
```

A regression can therefore be rolled back.

History can also be bisected.

Conceptually this becomes something like:

> **Git for operating-system evolution.**

User data such as `/home` remains persistent and is managed separately
from system generations.

------------------------------------------------------------------------

## 22. Provenance

Every meaningful system change should answer:

-   WHO caused this?
-   WHY?
-   WHICH GOAL?
-   WHICH OBSERVATION?
-   WHICH EVIDENCE?
-   WHICH AGENT?
-   WHICH MODEL?
-   WHICH CODE?
-   WHICH TESTS?
-   WHICH GENERATION?

Example:

``` text
system-change-991
├── goal G-192
├── observation O-481
├── proposal P-813
├── experiment E-551
├── commit 98a71c
├── benchmark B-992
├── review R-192
└── generation 185
```

This makes questions such as:

``` text
"Why did you change my build system?"
```

answerable with evidence rather than vague model recollection.

------------------------------------------------------------------------

## 23. Execution provenance

Important actions can also carry execution records.

``` text
Execution #91832

agent:
    Maintainer

goal:
    G-481

reason:
    Investigating failed indexing service

command:
    journalctl -u agent-indexer --since -10m

exit:
    0

duration:
    183 ms

stdout:
    artifact://...
```

The goal is not surveillance of every trivial syscall.

The goal is causal understanding of important autonomous actions.

------------------------------------------------------------------------

## 24. Capability and authority model

The capability system should not become a cage around Linux.

Its purpose is:

-   identity
-   authority
-   delegation
-   attribution
-   policy
-   transactionality
-   audit
-   consequence management

Different environments can permit different levels of autonomy.

### Evolution environment

Very high autonomy.

Agents may build, install, delete, restart, rewrite, experiment, and
recover.

### Stable system

High autonomy, but system-level changes should normally be versioned,
transactional, attributable, and reversible.

### External or irreversible effects

More deliberate policies are appropriate for actions whose consequences
cannot simply be rolled back.

Examples may include:

-   deleting external data
-   publishing publicly
-   spending money
-   transmitting credentials
-   modifying physical disks outside managed generations

The boundary is based on consequences, not on whether an operation feels
"AI-like."

------------------------------------------------------------------------

## 25. Self-development

Agentic OS agents should be able to work on Agentic OS itself.

The important architectural rule is:

> **Humans and agents use the same development pipeline.**

There should not be a fake "AI self-modification API" beside the real
project.

Agents should use:

-   Git
-   source trees
-   branches/worktrees
-   compilers
-   package builds
-   tests
-   CI
-   system image builds
-   QEMU/KVM
-   benchmarks
-   reviews
-   releases

Example:

``` text
Agentic OS source
        │
        ▼
agent creates branch/worktree
        │
        ▼
modifies userspace source
        │
        ▼
builds candidate
        │
        ▼
Generation N+1
        │
        ▼
boots candidate in KVM
        │
        ▼
tests + benchmarks
        │
        ▼
reviews evidence
        │
     ┌──┴──┐
     ▼     ▼
 reject  promote
```

This makes autonomous development ordinary software engineering rather
than magic.

------------------------------------------------------------------------

## 26. Self-improvement example

A user could eventually say:

``` text
"Improve yourself."
```

The system might then:

``` text
Goal G-1
   ↓
Inspect own system/source
   ↓
Find bottleneck or architectural weakness
   ↓
Create hypothesis
   ↓
Create development plan
   ↓
Create Git worktree
   ↓
Modify source
   ↓
Build candidate generation
   ↓
Boot candidate in KVM
   ↓
Run tests
   ↓
Benchmark
   ↓
Review evidence
   ↓
Accept / reject
   ↓
Create next generation
```

No human needs to manually move files between those stages.

But every stage remains observable and attributable.

------------------------------------------------------------------------

## 27. Model-provider abstraction

Agentic OS should not be permanently coupled to one model company or one
model architecture.

The core should expose provider-neutral concepts.

Conceptually:

``` text
reason()
tool_call()
structured_output()
context()
embedding()
```

Adapters can then target:

-   cloud models
-   local models
-   specialized models
-   future architectures

Different agents may use different models.

For example:

``` text
Planner       -> strong reasoning model
Builder       -> coding model
Local helper  -> small local model
Indexer       -> embedding model
Voice         -> speech model
```

The operating system remains the durable layer.

Models are replaceable computational components.

------------------------------------------------------------------------

## 28. Package and build philosophy

Agentic OS should not invent a new package format merely to appear
unique.

A better principle is:

> **Adopt proven Linux infrastructure until Agentic OS has a concrete
> reason to replace it.**

Early versions can bootstrap from an existing Linux package ecosystem
while Agentic OS provides its own:

-   generation model
-   provenance
-   evolution system
-   agent runtime
-   user model
-   adaptive experience

If agents and developers later discover that the package model
fundamentally limits evolution, a new package/environment architecture
can be developed from evidence.

------------------------------------------------------------------------

## 29. Initial technical direction

These are starting choices, not permanent laws.

### Platform

-   x86-64 first
-   UEFI
-   upstream stable Linux

### Base userspace

-   systemd
-   standard Linux tooling
-   normal shell environment

### Filesystem

A snapshot/generation-friendly filesystem is desirable.

Btrfs is a strong initial candidate.

### Desktop

-   Wayland
-   PipeWire
-   Agentic Shell layered on normal Linux graphical infrastructure

### Isolation and experimentation

-   namespaces
-   cgroups v2
-   seccomp
-   QEMU/KVM

### System implementation

Rust is a strong default for permanent privileged Agentic OS components.

Agents remain free to create appropriate software in:

-   Rust
-   C
-   C++
-   Python
-   shell
-   Java
-   other suitable languages

Python is useful for experimentation and orchestration but should not
automatically become the main privileged security boundary.

### Persistent state

SQLite is a practical initial choice for structured durable state.

Additional object stores, indexes, full-text search, embeddings, or
Git-backed artifacts can be added as requirements become clear.

### IPC

Use ordinary Linux mechanisms where appropriate:

-   Unix sockets
-   D-Bus
-   pipes
-   files
-   shared memory

Do not invent a new IPC mechanism without need.

------------------------------------------------------------------------

## 30. Suggested repository structure

``` text
agentic-os/
├── kernel/
│   ├── config/
│   └── packaging/
│
├── image/
│   ├── rootfs/
│   ├── initramfs/
│   └── build/
│
├── core/
│   ├── runtime/
│   ├── capabilities/
│   ├── goals/
│   ├── memory/
│   ├── observation/
│   ├── provenance/
│   └── evolution/
│
├── agents/
│   ├── planner/
│   ├── builder/
│   ├── researcher/
│   ├── reviewer/
│   ├── maintainer/
│   └── evolution/
│
├── shell/
│
├── providers/
│
├── evolution/
│   ├── vm/
│   ├── benchmarks/
│   ├── fitness/
│   └── promotion/
│
├── packaging/
│
├── schemas/
│
├── tests/
│   ├── boot/
│   ├── upgrade/
│   ├── integration/
│   └── evolution/
│
├── docs/
│   ├── ARCHITECTURE.md
│   ├── GOAL_MODEL.md
│   ├── CAPABILITIES.md
│   ├── EVOLUTION.md
│   ├── PROVENANCE.md
│   └── THREAT_MODEL.md
│
├── VISION.md
├── DECISIONS.md
└── JOURNAL.md
```

`kernel/` initially contains configuration and packaging, not an
evolving fork of Linux.

------------------------------------------------------------------------

## 31. Persistent project documents

Three documents are particularly important.

### VISION.md

What Agentic OS is trying to become.

This should preserve the long-term direction even while implementation
details change.

### DECISIONS.md

Architectural decisions.

Each important decision should include:

-   decision
-   alternatives
-   evidence
-   reasoning
-   consequences
-   whether it is reversible

Agents may later revise old decisions when evidence changes.

### JOURNAL.md

The engineering/evolution history.

Include:

-   experiments
-   failures
-   benchmarks
-   discoveries
-   unresolved questions
-   surprising behavior
-   next steps

This becomes useful context for both humans and future agents.

------------------------------------------------------------------------

## 32. Development roadmap

The project should be built incrementally, but each stage belongs to the
real distribution.

### Stage 0 --- Distribution foundation

Build a reproducible bootable Agentic OS image.

Requirements:

-   upstream Linux
-   UEFI boot
-   initramfs
-   root filesystem
-   networking
-   users
-   storage
-   systemd
-   logs
-   reproducible image builder
-   QEMU/KVM boot testing

### Stage 1 --- Agentic Core

Implement:

-   agent runtime
-   provider abstraction
-   durable state
-   observation
-   execution
-   provenance
-   ordinary Linux access

### Stage 2 --- Goal OS

Implement persistent:

-   Goals
-   Tasks
-   Findings
-   Decisions
-   Artifacts
-   Experiments

Agents can create and manage child goals.

### Stage 3 --- Generations

Create:

-   candidate generations
-   system manifests
-   rollback
-   activation
-   generation history
-   causal links to goals/experiments

### Stage 4 --- Evolution Lab

Automate:

``` text
snapshot/source
      ↓
candidate build
      ↓
KVM boot
      ↓
integration tests
      ↓
benchmarks
      ↓
evidence
      ↓
reject/promote
```

### Stage 5 --- Self-development

Agents can modify Agentic OS source and use the same build/test pipeline
as human developers.

### Stage 6 --- Agent society

Introduce specialized agents and structured delegation where useful.

### Stage 7 --- Adaptive experience

Build:

-   Agentic Shell
-   user model
-   adaptive workflows
-   adaptive UI
-   text input
-   voice input
-   progressive complexity

### Stage 8 --- Autonomous evolution

The system can identify opportunities without an explicit prompt.

Example:

``` text
Observation:
    repeated manual workflow

System:
    creates hypothesis

System:
    creates improvement goal

System:
    builds experiment

System:
    tests candidate

System:
    measures result

System:
    adopts or rejects change
```

------------------------------------------------------------------------

## 33. Agentic OS 0.1

Version 0.1 should be a real early distribution rather than disposable
demo software.

It does not need the complete vision.

But the pieces that exist should be engineered to survive.

A reasonable 0.1 target includes:

-   bootable/installable Linux distribution
-   upstream Linux kernel
-   networking
-   storage
-   users
-   recovery
-   update mechanism
-   basic graphical or text environment
-   Agentic Core starts automatically
-   provider-neutral model interface
-   persistent goals
-   persistent memory
-   system observation
-   shell/Linux execution
-   provenance
-   generations
-   rollback
-   Agentic OS source available to development agents
-   candidate generation builds
-   KVM testing
-   model-provider failure does not prevent basic OS operation

------------------------------------------------------------------------

## 34. Version direction

A possible progression:

``` text
0.1  Real distro + Agentic Core
 │
0.2  Goals + autonomous system operation
 │
0.3  Evolution engine
 │
0.4  Adaptive user environment
 │
0.5  Multi-agent specialization
 │
0.6  Autonomous projects and self-improvement
 │
 ...
 │
1.0  Daily-drivable self-evolving Agentic OS
```

The exact numbering is not important.

The architectural principle is:

> **0.1 should already assume that 1.0 will exist.**

------------------------------------------------------------------------

## 35. Security as systems architecture

Autonomy and security are not opposites.

The goal is not to make agents powerless.

The goal is to create environments where powerful autonomous behavior
has understandable consequences.

Useful mechanisms include:

-   identity
-   explicit authority
-   namespaces
-   cgroups
-   seccomp
-   VMs
-   snapshots
-   transactional changes
-   signed packages
-   provenance
-   generations
-   rollback
-   recovery environments
-   reproducible builds
-   audit trails

An agent in an Evolution VM can be extremely powerful because destroying
the VM is acceptable.

An agent operating the stable system can also be powerful, but important
system changes should generally create reversible generations.

The correct question is often not:

``` text
"Should the agent be allowed to do this?"
```

but:

``` text
"What is the consequence domain if this fails?"
```

------------------------------------------------------------------------

## 36. Reversibility

Reversibility should be a first-class property.

For an important autonomous change, the system should know:

-   what changed
-   what previous state existed
-   how to restore it
-   what data is persistent
-   whether rollback is complete
-   whether external effects occurred

Changes that cannot be reversed deserve different treatment from changes
that can.

This gives the system freedom to experiment aggressively where recovery
is cheap.

------------------------------------------------------------------------

## 37. Observability

The system cannot evolve intelligently if it cannot observe itself.

Useful observations include:

-   process behavior
-   service failures
-   CPU usage
-   memory pressure
-   disk usage
-   I/O
-   network behavior
-   boot time
-   application latency
-   build duration
-   repeated workflows
-   crashes
-   user corrections
-   accepted suggestions
-   rejected changes

Observations become evidence.

Evidence can create hypotheses.

Hypotheses can create goals.

------------------------------------------------------------------------

## 38. Learning from behavior without assuming intent

Observed behavior is useful but ambiguous.

Example:

``` text
Observation:
    User closes a notification immediately every day.
```

Possible interpretations:

-   notification is irrelevant
-   timing is bad
-   user already knows the information
-   notification style is annoying
-   user is temporarily busy

The OS should not immediately conclude:

``` text
"User hates all notifications."
```

Instead it can form a hypothesis, collect more evidence, make a
reversible change, or ask when uncertainty matters.

This is why explicit, observed, and inferred information must remain
separate.

------------------------------------------------------------------------

## 39. Experiment design

An evolution engine should behave more like an engineer than a random
mutator.

An experiment contains:

``` text
Experiment {
    id
    goal
    hypothesis
    candidate
    baseline
    metrics
    test_plan
    constraints
    result
    decision
}
```

Example:

``` text
Hypothesis:
    Build cache will reduce repeated project builds.

Baseline:
    median build = 82 s

Candidate:
    enable cache + configuration

Tests:
    clean build
    incremental build
    changed dependency
    reboot
    failure recovery

Result:
    incremental median = 19 s
    clean build unchanged
    disk +3.2 GB

Decision:
    accept
```

The system therefore learns through evidence rather than intuition
alone.

------------------------------------------------------------------------

## 40. History as causal graph

Traditional logs tell us what happened.

Agentic OS history should also tell us why.

``` text
User need
   ↓
Observation
   ↓
Hypothesis
   ↓
Goal
   ↓
Experiment
   ↓
Code change
   ↓
Build
   ↓
Benchmark
   ↓
Review
   ↓
Decision
   ↓
Generation
```

This causal graph is one of the most important long-term assets of the
system.

------------------------------------------------------------------------

## 41. Failure is useful data

Failed experiments should not disappear.

A rejected candidate can teach future agents:

-   what was attempted
-   why it failed
-   which benchmark regressed
-   what assumptions were wrong
-   which approaches should not be repeated blindly

Example:

``` text
Experiment E-201

goal:
    reduce indexing latency

attempt:
    parallelize database writes

result:
    throughput +12%
    corruption under shutdown test

decision:
    rejected

lesson:
    current transaction boundary cannot safely be parallelized
```

Future agents can retrieve this before trying the same idea.

------------------------------------------------------------------------

## 42. Adaptive applications

Over time, the distinction between "OS feature" and "application" may
become less rigid.

If the user repeatedly performs a workflow, an agent might create a
dedicated tool for it.

Example:

``` text
Repeated behavior
    ↓
agent identifies stable workflow
    ↓
creates small application/service
    ↓
tests it
    ↓
integrates it into user's environment
```

The user does not necessarily need to search an app store for every
missing workflow.

The system can create software for the individual.

------------------------------------------------------------------------

## 43. Adaptive abstractions

One of the deeper possibilities is that Agentic OS can create its own
higher-level APIs from repeated Linux operations.

Example:

An agent repeatedly diagnoses services using:

``` text
systemctl
journalctl
/proc
cgroups
network sockets
```

The OS notices the repeated pattern.

An agent then creates:

``` text
service.diagnose(name)
```

The abstraction is tested and becomes part of Agentic OS.

Future agents can use it.

Thus the distribution's abstractions emerge partly from real usage.

------------------------------------------------------------------------

## 44. Personal software evolution

Two installations might diverge dramatically.

### Installation A

Owner:

-   musician
-   visual thinker
-   dislikes terminals

Evolution:

-   audio-oriented workspace
-   visual automation
-   project-centered navigation
-   minimal exposed system complexity

### Installation B

Owner:

-   kernel developer
-   keyboard-driven
-   uses remote servers

Evolution:

-   terminal-heavy shell
-   advanced tracing
-   source navigation
-   custom build orchestration
-   system internals exposed prominently

Both remain Agentic OS.

The identity lies in the evolutionary architecture, not in one fixed
desktop layout.

------------------------------------------------------------------------

## 45. Distribution evolution from personal discoveries

Individual systems may discover useful mechanisms.

Those mechanisms can potentially flow upstream.

``` text
User-specific problem
        ↓
local agent solution
        ↓
generalization
        ↓
remove personal assumptions
        ↓
test across environments
        ↓
review
        ↓
Agentic OS upstream
```

This creates an interesting ecosystem where operating systems can learn
locally while software engineering determines what becomes globally
reusable.

------------------------------------------------------------------------

## 46. Human authority

The system is autonomous, but the human remains the source of values and
ownership.

The user should be able to:

-   inspect active goals
-   stop a goal
-   change priorities
-   reject adaptations
-   ask why something changed
-   rollback generations
-   define boundaries
-   correct false assumptions
-   request more or less autonomy
-   inspect system history

Autonomy should reduce micromanagement, not remove ownership.

------------------------------------------------------------------------

## 47. Children and protected policy layers

For child-oriented environments, some policy may need to exist outside
the ordinary self-evolution loop.

For example, guardian-defined constraints should not casually disappear
because an agent believes removing them would improve convenience.

This suggests separate governance domains:

``` text
Guardian / protected policy
           │
           ▼
      allowed space
           │
           ▼
     adaptive child OS
```

The adaptive experience can still evolve dramatically inside that space.

------------------------------------------------------------------------

## 48. What should not be hard-coded too early

Agentic OS should avoid prematurely deciding:

-   one permanent desktop metaphor
-   one permanent agent architecture
-   one permanent model provider
-   one permanent package model
-   one permanent memory backend
-   one permanent interaction method
-   one permanent set of agents
-   one permanent workflow abstraction

The project needs enough structure to evolve without freezing the
future.

------------------------------------------------------------------------

## 49. What should be stable

Not everything should be fluid.

Some infrastructure benefits from strong stability:

-   boot and recovery
-   system generations
-   provenance format
-   durable identity
-   storage integrity
-   security boundaries
-   update verification
-   data ownership
-   rollback mechanisms

The evolutionary system needs a reliable foundation from which to
experiment.

------------------------------------------------------------------------

## 50. Engineering principle: small scope, real quality

Early Agentic OS should be small.

But small does not mean fake.

If the project needs:

-   goals --- build a real minimal goal model
-   provenance --- model it properly
-   generations --- make them reproducible
-   rollback --- test it
-   model abstraction --- avoid provider lock-in
-   agent execution --- use real Linux

Do not create temporary architecture known in advance to be discarded.

The project can expand gradually while preserving its foundations.

------------------------------------------------------------------------

## 51. First major engineering milestone

The first truly important end-to-end capability is not voice or a fancy
desktop.

It is the system-development loop:

``` text
Agentic OS
   ↓
understands its own source/system
   ↓
creates a goal
   ↓
modifies userspace
   ↓
builds a candidate generation
   ↓
boots candidate in KVM
   ↓
tests it
   ↓
measures it
   ↓
reviews evidence
   ↓
promotes or rejects it
```

Once that mechanism exists, many future capabilities become applications
of the same architecture.

------------------------------------------------------------------------

## 52. Long-term evolution

The initial system evolves userspace.

Much later, evidence may justify deeper experimentation.

Possible distant research directions could include:

-   alternative init/service architecture
-   new package/environment systems
-   novel desktop protocols
-   agent-oriented IPC
-   new storage models
-   specialized local model runtimes
-   radically different application abstractions
-   eventually kernel experiments

But these should emerge from concrete needs.

The project does not need to reinvent the entire computing stack before
it can begin evolving.

------------------------------------------------------------------------

## 53. The central transformation

Traditional Linux distribution:

``` text
Developers
    ↓
design distribution
    ↓
release
    ↓
user configures it
```

Agentic OS:

``` text
Developers create seed architecture
          ↓
user installs it
          ↓
system observes real needs
          ↓
agents form goals
          ↓
system experiments
          ↓
system changes itself
          ↓
user behavior changes
          ↓
system adapts again
          ↓
continuous co-evolution
```

The distribution becomes a process rather than a static product.

------------------------------------------------------------------------

## 54. Definition

A concise definition of the project is:

> **Agentic OS is a self-evolving Linux distribution in which autonomous
> agents are first-class system actors capable of observing the machine
> and its owner, maintaining persistent goals and knowledge, developing
> and testing software, evolving the userspace through reversible
> generations, and continuously adapting the computing environment to
> the user's changing needs.**

------------------------------------------------------------------------

## 55. Project principles

1.  **Linux is the stable upstream substrate.**
2.  **Agentic OS is a real distribution, not an AI application.**
3.  **Agents are first-class Linux actors.**
4.  **Agents may use normal Linux interfaces directly.**
5.  **Structured APIs enhance Linux; they do not replace it.**
6.  **Persistent state belongs to the OS, not the model context.**
7.  **Goals are first-class persistent objects.**
8.  **Observations, hypotheses, experiments and decisions are
    preserved.**
9.  **Every important change should have provenance.**
10. **Humans and agents use the same software-development pipeline.**
11. **System changes should be generational and reversible.**
12. **Aggressive experimentation belongs in an isolated Evolution
    World.**
13. **Adaptation should follow actual needs rather than engagement
    metrics.**
14. **Explicit user statements must remain distinguishable from
    inference.**
15. **Complexity should be earned by use.**
16. **Evolution must be able to remove complexity as well as add it.**
17. **Fitness is multidimensional.**
18. **Model providers are replaceable components.**
19. **Proven Linux infrastructure should be reused until evidence
    justifies replacement.**
20. **The OS must remain operable when AI services are unavailable.**
21. **Start small, but build real architecture.**
22. **The system should eventually be capable of becoming something its
    original developers did not design.**

------------------------------------------------------------------------

## 56. Ultimate question

Agentic OS is ultimately exploring a different model of personal
computing.

If a computer has:

-   the flexibility of Linux
-   autonomous software agents
-   persistent memory
-   persistent goals
-   access to its own source
-   the ability to write software
-   the ability to create new system generations
-   isolated experimentation
-   measurements
-   rollback
-   provenance
-   a model of its owner
-   years of interaction history

then the question becomes:

> **What should that computer become for this particular human?**

Agentic OS should not attempt to hard-code the final answer.

Its purpose is to build the machinery through which the computer and its
owner can discover that answer over time.

------------------------------------------------------------------------

# End state

The long-term Agentic OS is not a fixed desktop with an AI button.

It is a **living, versioned, inspectable, reversible software
environment**.

It observes.

It learns.

It proposes.

It builds.

It tests.

It rejects bad ideas.

It preserves successful ones.

It simplifies itself.

It changes as its owner changes.

And beneath all of that remains a solid Linux system whose kernel
continues to evolve upstream.

That is the foundation of the transforming, evolutionary Agentic AI
Linux distribution.
