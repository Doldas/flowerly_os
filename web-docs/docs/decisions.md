# Decisions

This is an index of the major decisions behind Flowerly OS. Each entry links to
its Architecture Decision Record in [adr/](adr/).

- **Rust is the primary system language** —
  [ADR-0001](adr/0001-rust-as-primary-system-language.md).
- **Go is reserved for distributed/orchestration-heavy services** —
  [ADR-0002](adr/0002-go-for-distributed-orchestration.md).
- **Python is for research, experiments and temporary tooling** —
  [ADR-0003](adr/0003-python-for-experimentation.md).
- **Upstream Linux remains the kernel substrate** —
  [ADR-0004](adr/0004-upstream-linux-kernel-policy.md).
- **SQLite is the initial durable structured state store** —
  [ADR-0005](adr/0005-sqlite-as-initial-state-store.md). Its original
  per-service-file topology is superseded by ADR-0011.
- **systemd is the initial service supervisor** —
  [ADR-0006](adr/0006-systemd-service-supervision.md).
- **QEMU/KVM is the Evolution World** —
  [ADR-0007](adr/0007-kvm-evolution-world.md).
- **Flows are first-class persistent OS objects** —
  [ADR-0008](adr/0008-flows-as-first-class-os-objects.md).
- **The product is named Flowerly OS; internal runtime names remain agentic-***
  — [ADR-0009](adr/0009-project-name-flowerly-os.md).
- **Local service IPC is versioned length-prefixed JSON over Unix sockets** —
  [ADR-0010](adr/0010-unix-socket-json-ipc.md).
- **Local structured state uses one shared `state.db` by default** —
  [ADR-0011](adr/0011-shared-local-state-database.md). Service/domain ownership
  stays logical while local causal state shares one ACID store.
- **`agenticctl` is a thin human/agent control plane** —
  [ADR-0012](adr/0012-agenticctl-control-plane.md).
- **Conceptual domains do not automatically become daemons** —
  [ADR-0013](adr/0013-process-boundaries-require-justification.md). New process
  boundaries require a concrete lifecycle, isolation, security or runtime reason.
- **Linux authority remains the enforcement substrate beneath Flowerly policy** —
  [ADR-0014](adr/0014-linux-authority-is-the-enforcement-substrate.md).
- **Arch Linux is the initial userspace/package substrate** —
  [ADR-0015](adr/0015-arch-linux-userspace-substrate.md). Rolling upstream
  changes feed candidate generations; Stable World does not roll directly.

Proposed architecture that is not yet a committed decision lives under
[RFCs](rfcs/), including the Intent domain and future Flow state decomposition.
