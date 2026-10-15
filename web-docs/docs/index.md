# Flowerly OS Documentation

Welcome to the public documentation and help portal for **Flowerly OS** — the self-evolving Linux distribution internally developed around the Agentic OS architecture.

!!! note "Early development"
    Flowerly OS is still foundational software. The documentation distinguishes implemented behavior from planned architecture.

## Where to start

- **New user or tester:** start with [Online Help](help/index.md).
- **Developer:** read [Getting Started](help/getting-started.md), [Architecture](architecture.md), and [Build Guide](build.md).
- **Project planning:** see the [Roadmap](roadmap.md), [Release Model](releases.md), and [Project Overview](project/index.md).
- **Deep architecture:** browse the [Wiki](wiki/index.md).

## Core ideas

Flowerly OS is built around persistent **Flows**, durable **Goals**, persistent **Sessions**, reversible **Generations**, explicit **Provenance**, and an isolated KVM-based **Evolution World**.

The system must remain a functional Linux environment without an AI provider. AI is a system capability, not a boot dependency.

## Current status

The repository currently contains the Rust workspace, shared runtime crates, the control-plane CLI, multiple service skeletons, SQLite persistence and migrations, and local Unix-socket IPC. Persistent PTY sessions, generation building, KVM evolution, the full agent runtime, and self-modification remain roadmap work.

See [Roadmap](roadmap.md) for the authoritative execution sequence.
