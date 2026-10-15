# 0004: Upstream Linux as the Kernel Substrate

Date: 2026-10-11
Status: Accepted

## Context

Flowerly OS evolves userspace, services, agents, workflows and operating
abstractions. It depends on ordinary Linux facilities: systemd, cgroups v2,
namespaces, seccomp, `/proc`, `/sys`, netlink, sockets, pidfds, PTYs, D-Bus,
PipeWire, Wayland, QEMU/KVM and containers. Maintaining a kernel fork at this
stage would add enormous maintenance and security burden without enabling the
initial goals.

## Decision

Use upstream Linux as the kernel substrate. Do not fork or patch the kernel for
the initial architecture. Concrete kernel configurations live in `kernel/config`
and are consumed by the image build; kernel packaging lives in
`kernel/packaging`. Kernel changes are adopted through reproducible packaging and Flowerly's
generation/update policy, and the project cooperates with upstream and honors
GPL obligations (Flowerly OS is licensed GPL-3.0-or-later). ADR-0015 selects
Arch Linux as the initial userspace/package substrate; Arch's `linux` package
may bootstrap early images, but it does not replace this upstream-kernel policy.

## Consequences

- Security and hardware support track upstream Linux instead of a fork.
- Structured Agentic APIs are abstractions over Linux, not replacements for it.
- Kernel configuration is explicit and reproducible rather than implicit.
- Some desired behavior may need userspace workarounds until upstream support
  exists; those trade-offs are recorded rather than hidden.
- Introducing any kernel fork requires a superseding ADR and an RFC.
