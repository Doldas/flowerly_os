# 0001: Rust as the Primary System Language

Date: 2026-10-11
Status: Accepted

## Context

Flowerly OS is a long-lived, self-evolving Linux distribution. Its trusted
system services must run continuously, handle untrusted input, and never corrupt
durable state. The runtime needs memory safety without a garbage collector,
predictable performance, sound concurrency, strong typing shared across
components, and a healthy ecosystem for Linux integration.

## Decision

Rust is the default language for permanent system software: the core daemons,
storage, IPC, provenance, capabilities, generation management and privileged
integration. The repository is a Cargo workspace pinned to the `stable` channel
in `rust-toolchain.toml`, with an intentional minimum supported Rust version
recorded in `Cargo.toml`. `unsafe` is avoided; every unavoidable `unsafe` block
must carry a short safety explanation. Rust is the reason the typed IDs and
shared domain model live in `crates/agentic-types`.

## Consequences

- Memory and thread safety without a GC; no runtime pauses inside daemons.
- Mostly static, single-binary services that are simple to package and supervise.
- Shared types across the workspace keep IDs and state transitions consistent.
- Steeper learning curve and slower compile times than dynamic languages.
- Some areas of the ecosystem are less mature and need careful dependency review.
- Changing the primary language requires an RFC and a superseding ADR; see
  `docs/LANGUAGE_POLICY.md` and `../docs/LANGUAGE_POLICY.md`.
