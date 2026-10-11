# Decisions

This is an index of the major decisions behind Flowerly OS. Each entry
summarizes the decision and links to its Architecture Decision Record in
[adr/](adr/). Full ADRs record the alternatives considered, the evidence, the
consequences, and whether the decision is reversible.

The ADR files below are planned; the numbers are reserved and stable.

- **Rust is the primary system language** — [ADR-0001](adr/)
  (`adr/0001-rust-as-primary-system-language.md`). Rust is the default for
  trusted, long-lived OS services and the core runtime: memory safety without a
  garbage collector, predictable concurrency, and strong typing for durable
  system boundaries.
- **Go for distributed and orchestration services** — [ADR-0002](adr/)
  (`adr/0002-go-for-distributed-services.md`, planned). Go is preferred for
  networking-heavy, remote-orchestration, and synchronization services. It does
  not replace Rust system services without a concrete reason.
- **Python for research and prototyping** — [ADR-0003](adr/)
  (`adr/0003-python-for-research.md`, planned). Python is used for AI/ML
  experiments, analysis, benchmarks, evaluation harnesses, and temporary tools.
  A successful prototype may later become Rust or Go.
- **Upstream Linux is the kernel substrate** — [ADR-0004](adr/)
  (`adr/0004-upstream-linux-substrate.md`). Flowerly OS follows stable upstream
  Linux and does not fork the kernel. Innovation happens above the kernel.
- **SQLite is the initial state store, with migrations** — [ADR-0005](adr/)
  (`adr/0005-sqlite-initial-state-store.md`). SQLite is the durable structured
  store from day one, and all schema changes go through ordered migrations; the
  production schema is never edited by hand.
- **systemd is the initial service supervisor** — [ADR-0006](adr/)
  (`adr/0006-systemd-service-supervisor.md`). Flowerly OS services are ordinary
  systemd units; supervision, restart policy, dependency ordering, cgroups, and
  logging are not rebuilt.
- **KVM is the initial Evolution World** — [ADR-0007](adr/)
  (`adr/0007-kvm-evolution-world.md`). Aggressive experiments and candidate
  generations are tested in QEMU/KVM, separate from the stable system.
- **Flows are first-class persistent OS objects** — [ADR-0008](adr/)
  (`adr/0008-flows-first-class.md`). A Flow is a persistent, evolvable activity
  context, not a window layout or a configuration profile.
- **The product is named "Flowerly OS"; runtime components keep `agentic-*`
  names** — [ADR-0009](adr/) (`adr/0009-project-name-flowerly-os.md`). The
  human-facing name is Flowerly OS; crates, services, and IDs retain the
  internal `agentic-*` naming defined by the architecture specification.
- **Unix-domain-socket JSON IPC (protocol v1)** — [ADR-0010](adr/)
  (`adr/0010-unix-socket-json-ipc.md`, planned). Local services speak a
  length-prefixed JSON protocol over Unix domain sockets: simple, inspectable,
  and versionable. No distributed RPC platform is invented early.
- **Per-service SQLite databases** — [ADR-0011](adr/)
  (`adr/0011-per-service-databases.md`, planned). Each service owns its own
  database, keeping service boundaries and failure domains clear.
- **`agenticctl` is the human/agent control plane** — [ADR-0012](adr/)
  (`adr/0012-agenticctl-control-plane.md`, planned). `agenticctl` is a thin
  client over the same IPC API the future GUI shell will use; no privileged
  logic lives in it.
