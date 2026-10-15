# Flowerly OS — Language Policy

Flowerly OS uses a small number of languages, each for a reason. The goal is a
trusted, inspectable, low-dependency core with cheap experimentation around it.

## Rust — primary system language

Rust is the default language for permanent system software: trusted daemons,
storage, IPC, provenance, capabilities, generation management, privileged
integration and system APIs. It gives memory and thread safety without a garbage
collector, predictable performance, and mostly static single-binary services.
The workspace is pinned to `stable` in `rust-toolchain.toml`. `unsafe` is
avoided; every unavoidable `unsafe` block carries a safety comment.
See [ADR-0001](adr/0001-rust-as-primary-system-language.md).

## Go — distributed and orchestration services (planned)

Go is the preferred language for future networking- and coordination-heavy
services: remote execution, Flow sync, cluster coordination, model routing and
machine-to-machine APIs. It is **not used yet**; a Go service is introduced only
through an RFC that justifies the choice. Go must not replace a Rust system
service without a concrete reason.
See [ADR-0002](adr/0002-go-for-distributed-orchestration.md).

## Python — research, prototyping and tooling

Python is for AI/ML experiments, data analysis, benchmarks, log analysis,
prototype agents, migration tools, evaluation harnesses and temporary scripts,
under `python/`. A useful prototype that becomes permanent infrastructure is
reimplemented in Rust or Go. Python is never used for the core runtime or
security-critical paths.
See [ADR-0003](adr/0003-python-for-experimentation.md).

## C — low-level only when justified

C is allowed only where direct Linux ABI or libbpf/eBPF integration clearly
benefits, an existing C library is the right foundation, or a very small
component is simpler in C. It is not the default for new services.

## Java — ecosystem-specific

Java is allowed only where a JVM ecosystem provides a concrete advantage. It is
never the default OS-core language.

## Introducing a new language

A new language, runtime or toolchain is a consequential architectural change:

1. Write an RFC (`rfcs/`) describing the need and why the existing languages are
   unsuitable.
2. Get it accepted, then record the durable decision as an ADR in `adr/`.
3. The introduction must be justified by a concrete component, not curiosity.
4. The language must be integrated into CI (lint + test) before code merges.
5. It must not silently change this policy; changing the policy requires an ADR.

The default answer to "can we use another language?" is no until a real need is
demonstrated.
