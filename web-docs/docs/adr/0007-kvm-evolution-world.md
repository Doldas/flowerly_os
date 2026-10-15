# 0007: KVM as the Initial Evolution World

Date: 2026-10-11
Status: Accepted

## Context

Self-evolution requires running candidate system generations, destructive tests
and aggressive experiments. Doing that on the machine the user depends on would
risk the Stable World. A hypervisor boundary gives strong isolation and lets
experiments fail safely, while still booting a real Linux system.

## Decision

Use QEMU with KVM acceleration as the initial Evolution World virtualization
layer. Candidate generations are built, booted, health-checked, integration-
tested, shut down and evaluated inside the Evolution World; results become
experiment records. The workflow is exposed through `just vm-test`. The
Evolution World is the sandbox in which agents may act with high autonomy,
because a bad experiment cannot harm the Stable World.

## Consequences

- Experiments are isolated from the user's real environment and data.
- A real Linux boot is tested, not a simulation, improving confidence.
- KVM requires hardware virtualization support and QEMU on the developer host.
- Slow VM boot and test cycles compared with local unit tests.
- Implemented today: `evolutiond` is a stub exposing a health contract.
- Planned: the KVM boot harness, `just vm-test`, benchmarks and promotion.
- QEMU is not yet installed in the current development environment.
