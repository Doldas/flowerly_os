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
evolve the userspace around the actual needs of its owner. Flowerly uses Arch
Linux as its initial userspace/package substrate while retaining an upstream
Linux kernel policy. Arch supplies moving upstream inputs; Flowerly generations
decide what reaches Stable World.

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

## Proposed future deployment model

Flowerly may eventually support **location-independent operation**: the user interface on a laptop/tablet/browser, Flow and Session runtime on a local workstation or remote Linux VM/server, and AI models on local hardware, an organizational inference service or explicitly approved provider APIs. Users would choose human-readable AI capability profiles and expert-configurable per-agent resource/model policies; organizations could administer shared defaults and security ceilings. These are **proposals, not implemented features**. See [RFC 0004](docs/rfcs/0004-location-independent-flowerly-ai-compute.md); current 0.0.4 work still focuses on truthful local PTY/session ownership and release gates.

## Current status

Sprint 0 and Sprint 1 are substantially complete at version 0.0.1. Implemented
today:

- A Rust Cargo workspace with the shared crates `agentic-types`,
  `agentic-storage`, `agentic-ipc`, and `agentic-core`.
- Rust services: `agenticd`, `flowd`, `sessiond`, `goald`, `provenanced`,
  `capabilityd`, `generationd`, and `evolutiond`.
- The `agenticctl` control-plane CLI.
- A shared local SQLite `state.db` with ordered, concurrent-safe migrations
  and a separate durable state directory.
- Unix-domain-socket, length-prefixed JSON IPC (protocol v1).

Planned, not yet implemented: PTY-backed persistent sessions, the agent
runtime, the KVM Evolution World, generation building, bootable Arch-based
Flowerly images, and self-modification. The initial Arch image-input scaffold
already lives under `image/arch/`; it is not yet a bootable release image. See
[docs/roadmap.md](docs/roadmap.md).

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
| `services/` | Daemons (`agenticd`, `flowd`, `sessiond`, `goald`, `provenanced`, `capabilityd`, `generationd`, `evolutiond`) |
| `tools/` | `agenticctl` control-plane CLI |
| `docs/` | Architecture specifications, design updates, ADRs, RFCs, specs |
| `scripts/` | Development tooling |
| `image/` | Arch Linux rootfs/image inputs and future generation-image builder |

## Language policy

- **Rust** is the default for trusted, long-lived OS services and the core
  runtime.
- **Go** is preferred for distributed and orchestration-heavy services.
- **Python** is used for research, experiments, analysis, and temporary tools.
- **C** is used only where direct low-level Linux integration justifies it.
- **Java** is allowed only where a JVM ecosystem provides a concrete advantage.

## Documentation website

The public wiki, online help, architecture reference, roadmap, and release documentation are published through GitHub Pages:

**https://doldas.github.io/flowerly_os/**

The website is built from the Markdown under `docs/`, so repository documentation and online help stay in the same version-controlled source of truth.

## Project documents

- [docs/vision.md](docs/vision.md) — long-term direction.
- [docs/architecture.md](docs/architecture.md) — what exists today and the target.
- [docs/roadmap.md](docs/roadmap.md) — the execution plan.
- [docs/decisions.md](docs/decisions.md) — index of major decisions.
- [docs/journal.md](docs/journal.md) — chronological engineering notes.
- [CHANGELOG.md](CHANGELOG.md) — release history.
- [CONTRIBUTING.md](CONTRIBUTING.md) — how to contribute.
- [SECURITY.md](SECURITY.md) — reporting and security policy.
- [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) — community expectations.
- [docs/](docs/) — specifications and design updates.
- [docs/adr/](docs/adr/) — Architecture Decision Records.

## Release model

Releases are semantic-ish and pre-1.0. `0.0.x` marks foundational internal
capability; `0.1.0` will be the first coherent, real Flowerly OS development
release. Each release carries a source tag, release notes, build manifest, test
summary, known limitations, and artifact checksums. See [docs/roadmap.md](docs/roadmap.md).

## License

GPL-3.0-or-later.
