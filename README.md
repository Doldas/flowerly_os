# Flowerly OS

Flowerly OS (internally "Agentic OS") is a self-evolving Linux distribution in
which autonomous agents are first-class system actors. Persistent goals, flows,
and sessions are first-class objects, system changes become reversible
generations, aggressive experiments run in a KVM Evolution World, and every
meaningful autonomous change carries provenance.

This repository is the source of the distribution, its system services, and the
tooling that both humans and agents use to build it. It is an early foundation:
most of the long-term direction is planned, not yet implemented.

## What is Flowerly OS

A Linux distribution whose defining property is that the operating environment
itself can evolve. Agents observe the system, pursue persistent goals, operate
Linux directly, develop software, build and test candidate generations, and
evolve the userspace around the actual needs of its owner. Linux remains the
stable substrate; the innovation happens above the kernel.

## Design principles

1. The OS must boot and remain administrable without an AI provider.
2. Build on existing Linux primitives; wrap them, do not replace them.
3. Persistent goals, flows, and sessions are first-class objects.
4. System changes become reversible generations.
5. Aggressive experiments run in a KVM Evolution World.
6. Every meaningful autonomous change carries provenance.
7. Humans and agents use the same git/build/test/release pipeline.
8. AI providers are replaceable and provider-neutral.
9. Complexity is earned by use; the environment grows with the user.
10. Evolution is evidence-driven, and failed experiments remain useful history.

## Current status

Sprint 0 and Sprint 1 are substantially complete at version 0.0.1. Implemented
today:

- A Rust Cargo workspace with the shared crates `agentic-types`,
  `agentic-storage`, `agentic-ipc`, and `agentic-core`.
- Rust services: `agenticd`, `flowd`, `sessiond`, `goald`, `provenanced`,
  `capabilityd`, `generationd`, and `evolutiond`.
- The `agenticctl` control-plane CLI.
- SQLite persistence with an ordered migration mechanism.
- Unix-domain-socket, length-prefixed JSON IPC (protocol v1).

Planned, not yet implemented: PTY-backed persistent sessions, the agent
runtime, the KVM Evolution World, generation building, and self-modification.
See [ROADMAP.md](ROADMAP.md).

## Getting started

Prerequisites: Linux x86-64, a stable Rust toolchain (`rustup`), and `just`.

```sh
just build    # compile the whole workspace (release)
just check    # fast compile check of all targets
just lint     # rustfmt check + clippy (warnings as errors)
just test     # run the full test suite
```

### Quick smoke test

Build, start the whole local runtime, and query it through the control plane:

```sh
just build
just serve    # starts all daemons against a dev runtime directory
just status   # prints structured status for every service
just stop     # shuts the dev runtime down cleanly
```

## Repository layout

| Path | Contents |
| --- | --- |
| `crates/` | Shared Rust crates: `agentic-types`, `agentic-storage`, `agentic-ipc`, `agentic-core` |
| `services/` | Daemons (`agenticd`, `flowd`, `sessiond`, `goald`, `provenanced`, `capabilityd`, `generationd`, `evolutiond`) and `agenticctl` |
| `docs/` | Architecture specifications and design updates |
| `adr/` | Architecture Decision Records |
| `rfcs/` | Design proposals under discussion |
| `evolution/` | KVM Evolution World, benchmarks, promotion (planned) |
| `image/` | Root filesystem, initramfs, installer (planned) |
| `kernel/`, `packaging/` | Kernel configuration (planned); systemd units and distribution packaging (planned) |
| `tests/` | Unit, integration, boot, upgrade, generation, and evolution tests |
| `go/`, `python/`, `c/`, `shell/`, `schemas/` | Language-specific and schema areas for future components |

## Language policy

- **Rust** is the default for trusted, long-lived OS services and the core
  runtime.
- **Go** is preferred for distributed and orchestration-heavy services.
- **Python** is used for research, experiments, analysis, and temporary tools.
- **C** is used only where direct low-level Linux integration justifies it.
- **Java** is allowed only where a JVM ecosystem provides a concrete advantage.

## Project documents

- [VISION.md](VISION.md) — long-term direction.
- [ARCHITECTURE.md](ARCHITECTURE.md) — what exists today and the target.
- [ROADMAP.md](ROADMAP.md) — the execution plan.
- [DECISIONS.md](DECISIONS.md) — index of major decisions.
- [JOURNAL.md](JOURNAL.md) — chronological engineering notes.
- [CHANGELOG.md](CHANGELOG.md) — release history.
- [CONTRIBUTING.md](CONTRIBUTING.md) — how to contribute.
- [SECURITY.md](SECURITY.md) — reporting and security policy.
- [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) — community expectations.
- [docs/](docs/) — specifications and design updates.
- [adr/](adr/) — Architecture Decision Records.

## Release model

Releases are semantic-ish and pre-1.0. `0.0.x` marks foundational internal
capability; `0.1.0` will be the first coherent, real Flowerly OS development
release. Each release carries a source tag, release notes, build manifest, test
summary, known limitations, and artifact checksums. See [ROADMAP.md](ROADMAP.md).

## License

GPL-3.0-or-later.
