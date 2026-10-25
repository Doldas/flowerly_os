# Architecture Decision Records

Architecture Decision Records (ADRs) capture consequential technical decisions
and the reasoning behind them.

## Accepted decisions

1. [Rust as the primary system language](0001-rust-as-primary-system-language.md)
2. [Go for distributed and orchestration services](0002-go-for-distributed-orchestration.md)
3. [Python for experimentation](0003-python-for-experimentation.md)
4. [Upstream Linux kernel policy](0004-upstream-linux-kernel-policy.md)
5. [SQLite as the initial state store](0005-sqlite-as-initial-state-store.md) — per-service physical DB topology superseded by ADR-0011
6. [systemd service supervision](0006-systemd-service-supervision.md)
7. [KVM Evolution World](0007-kvm-evolution-world.md)
8. [Flows as first-class OS objects](0008-flows-as-first-class-os-objects.md)
9. [Project name: Flowerly OS](0009-project-name-flowerly-os.md)
10. [Unix-socket JSON IPC](0010-unix-socket-json-ipc.md)
11. [One shared local SQLite state database](0011-shared-local-state-database.md)
12. [agenticctl control plane](0012-agenticctl-control-plane.md)
13. [Process boundaries require justification](0013-process-boundaries-require-justification.md)
14. [Linux authority is the enforcement substrate](0014-linux-authority-is-the-enforcement-substrate.md)
15. [Arch Linux as the initial userspace substrate](0015-arch-linux-userspace-substrate.md)
16. [AI compute layer is a flowerlyd module until enforcement evidence exists](0020-ai-compute-layer-is-a-module.md)
17. [Reference inference provider runs as a fault-isolated process](0021-provider-fault-isolation.md)
18. [Supervised Linux PTY Session ownership](0016-pty-session-supervision.md)
    — accepted by the owner 2026-10-25 (#73); containment and Arch VM
    verification remain release gates for #50, #72 and 0.0.4.

## Proposed decisions under review

- [0017: Unified and mode-driven storage subsystem](0017-unified-mode-driven-storage.md)
  — proposed dual-core storage and Standalone/Cluster/CoreOnly modes;
  ADR-0011 remains the accepted live topology pending migration review.

- [0018: Strict Boundary Isolation Layer (Layer 1.5)](0018-strict-boundary-isolation-layer.md)
  — proposed kernel/driver facade boundaries, subject to security
  review and ADR-0017 compatibility.

New architecture decisions should be added as numbered ADRs rather than silently
changing foundational policy. Proposed, not-yet-accepted designs belong in
[../rfcs/](../rfcs/).
