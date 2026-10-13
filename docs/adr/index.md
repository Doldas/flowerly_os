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

## Proposed decisions under review

- [0016: Supervised Linux PTY Session ownership](0016-pty-session-supervision.md)
  — pending issue #50 release and security acceptance; not yet an accepted ADR.

New architecture decisions should be added as numbered ADRs rather than silently
changing foundational policy. Proposed, not-yet-accepted designs belong in
[../rfcs/](../rfcs/).
