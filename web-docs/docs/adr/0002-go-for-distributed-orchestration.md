# 0002: Go for Distributed and Orchestration Services

Date: 2026-10-11
Status: Accepted

## Context

The initial Flowerly OS is single-machine and local-first; Rust covers the
trusted local runtime. Some future components are primarily network- and
coordination-shaped: remote execution gateways, Flow synchronization, cluster
coordination, model routing and machine-to-machine APIs. These benefit from
idiomatic concurrency, a strong standard networking library, and simple
deployment.

## Decision

Go is the preferred language for future distributed and orchestration services
and will live under `go/`. Go is not used yet and no Go service exists in the
current milestone. A Go component is introduced only through a deliberate RFC
that names the component, shows why it is distribution- or orchestration-heavy,
and justifies choosing Go over Rust. Rust remains the default for local trusted
system services; Go must not replace a Rust system service without a concrete
reason.

## Consequences

- Idiomatic concurrency and networking make distributed services cheap to build.
- Static binaries simplify packaging and operations for edge services.
- A clear split emerges: Rust for the trusted local core, Go for distributed
  edges and synchronization.
- A second toolchain, lint path and CI matrix must be maintained once Go lands.
- The RFC gate is the main protection against architectural drift.
- The full policy is documented in `../docs/LANGUAGE_POLICY.md`.
