# 0015: Arch Linux as the Initial Userspace Substrate

Date: 2026-10-11  
Status: Accepted

## Context

Flowerly OS needs a real Linux userspace long before it makes sense to maintain
its own libc, core utilities, package repository, installer stack, or general
purpose distribution infrastructure.

The userspace base must support the project's actual direction:

- stay close to upstream Linux and modern userspace projects;
- expose new kernel, systemd, Mesa, QEMU, compiler, firmware and hardware
  support without long distribution delays;
- remain understandable through ordinary Linux tools;
- provide a large package ecosystem;
- work with the Flowerly generation model rather than becoming the generation
  model itself.

Arch Linux is rolling-release. Stable World, however, must be reversible and
must not mutate continuously underneath the user. Treating a rolling
distribution as the Stable World update mechanism would conflict with Flowerly's
architecture.

## Decision

Flowerly OS uses **Arch Linux as its initial userspace and package substrate**.

Arch provides upstream packages and package metadata. It does not define the
identity, release cadence, or state-management semantics of Flowerly OS.

The governing update rule is:

> **Arch rolls continuously. Flowerly Stable World does not.**

Repository changes are inputs to a candidate generation:

```text
Arch repositories change
        ↓
resolve candidate package set
        ↓
record exact versions and package provenance
        ↓
build candidate generation
        ↓
Evolution World tests and measures it
        ↓
promote or reject
        ↓
Stable World changes generation atomically
```

A normal unmanaged `pacman -Syu` against Stable World is therefore not the
long-term Flowerly update path. Pacman remains a normal Linux tool and package
resolver, but system-wide upgrades intended to change Stable World are performed
while constructing candidate generations.

Arch's full-upgrade model is compatible with this design: the candidate is
resolved and tested as one coherent package set rather than relying on partial
upgrades.

## Kernel policy

ADR-0004 remains in force. Flowerly uses upstream Linux and does not create a
kernel fork merely because Arch is the userspace substrate.

Early VM images may use Arch's `linux` package to get a working boot path
quickly. The target is a reproducible Flowerly kernel build/package derived from
upstream kernel.org sources plus an explicit Flowerly configuration. Kernel
patches require separate evidence and a superseding architecture decision.

## Reproducibility

A package-name list is only a build input. A promoted generation must eventually
record the resolved result, including at least:

- repository/snapshot identity or equivalent acquisition context;
- package name, version and architecture;
- package content/checksum identity;
- kernel source/version, config hash and produced artifact hash;
- Flowerly source commit and produced package/binary hashes;
- parent generation and causal provenance.

Flowerly may later maintain its own cache or repository snapshot containing the
exact package artifacts used by promoted generations. That is preferred over
assuming a rolling upstream will retain every historical artifact forever.

## Image tooling

The first image work is VM-first and minimal. The repository starts with an
Arch image-input scaffold under `image/arch/`.

mkosi is the preferred initial image assembler because it can construct Arch
images while keeping the configuration declarative. `pacman`/Arch package
metadata remain the underlying package source. The image tooling is replaceable;
generation manifests and reproducible inputs are the architectural contract.

The initial scaffold does not claim that Flowerly already produces a bootable
release image. Bootable image generation remains a roadmap deliverable and must
be exercised by KVM tests before being called implemented.

## Consequences

- Flowerly gains current upstream userspace and hardware enablement without
  maintaining a general-purpose distribution from scratch.
- Rolling changes become useful candidate-generation stimuli rather than direct
  mutations of the user's stable environment.
- The generation builder must freeze and record exact resolved package state.
- Rollback belongs to Flowerly generations, not to pacman transaction history.
- Users and agents still have access to ordinary Arch/Linux tools.
- Distribution-specific logic must stay below Flowerly's domain model so a
  future substrate change remains possible.
