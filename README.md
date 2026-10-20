# Flowerly OS

Flowerly OS is an experimental, agent-oriented Linux distribution founded on
Arch Linux, persistent goals and flows, durable sessions, reversible system
generations, and evidence-based provenance. Basic administration must work
without any AI provider.

## Distribution-first architecture

The target layout replaces eight lightweight OS daemons with a unified
**`flowerlyd`** process and a separate, restricted **`evolutiond`** KVM
boundary. The `agenticctl` command line tool remains available to humans and
agents.

```text
flowerly_os/
├── distro/                 Arch base-image, target overlay and profiles
├── src/
│   ├── flowerlyd/          Flow, Goal, Session, Capability, Generation,
│   │                       Provenance and Execution modules in one process
│   ├── evolutiond/         separate high-privilege VM boundary (stub)
│   └── shared/flowerly-core/  types, SQLite, framed JSON IPC and daemon support
├── tools/
│   ├── agenticctl/         human and agent control CLI
│   └── distro-builder/     image input validator and future image builder
├── testbed/                integration, KVM seeds and test fixtures
└── web-docs/               MkDocs source, architecture, RFCs and ADRs
```

`flowerlyd` owns the `flowerlyd.sock` CLI control plane. Its flow,
generation and provenance workers use bounded Tokio channels in memory,
instead of separate daemon sockets. The external protocol remains
length-prefixed JSON v1 over authenticated same-UID Unix sockets.
`evolutiond` uses a different account, runtime directory and socket.

The SQLite schema and existing durable migration sequence are preserved.
Linux PTY guardians remain independently supervised child processes for
containment, **not** separate long-running Flowerly service binaries.

## Build and run

Linux, `rustup` and `just` are required. The repository pins Rust 1.99.1
(workspace MSRV 1.99) and aims to track the latest stable Rust release.
`rustup` selects the pinned compiler automatically. Run:

```sh
just check
just lint
just test
just build
just serve            # start flowerlyd and evolutiond for local development
just status
./target/release/agenticctl flow list
just stop
```

For image definitions:

```sh
cargo run -p distro-builder -- validate
```

## Implemented versus planned

This architecture migration is subject to pull-request CI and real process
tests. The current `evolutiond` has **no working KVM hypervisor**, and the
distro builder does **not** produce bootable Arch images. Generation promotion,
remote authenticated sessions, and AI-controlled system modification remain
roadmap work. No provider selection confers OS privileges.

The architecture, roadmap, design decisions and release limitations are in
[web-docs/docs/](web-docs/docs/) and especially
[web-docs/docs/architecture.md](web-docs/docs/architecture.md).
The public docs site is https://doldas.github.io/flowerly_os/.

## License

GPL-3.0-or-later.
