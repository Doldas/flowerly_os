# Flowerly OS — Image and Userspace Model

Flowerly OS uses Arch Linux as its initial userspace/package substrate while
retaining Flowerly-specific generation, promotion, rollback and provenance
semantics. See [ADR-0015](adr/0015-arch-linux-userspace-substrate.md).

## Layering

```text
Flowerly services, Flows, Goals, Sessions, agents
                     ↓
Flowerly generations / provenance / policy
                     ↓
Arch Linux userspace and package ecosystem
                     ↓
upstream Linux kernel
                     ↓
hardware
```

Arch provides ordinary Linux packages. It does not define the Stable World
update model.

## Rolling upstream, stable generations

Arch may change every day. Stable World changes only through a promoted
generation:

```text
Arch repo delta
    ↓
resolve full candidate package set
    ↓
freeze exact inputs
    ↓
build candidate rootfs/image
    ↓
boot/test/measure in Evolution World
    ↓
ACCEPT → promote generation
REJECT → keep current Stable World
```

This makes a rolling upstream useful without making the user's operating
environment uncontrolled.

## Build inputs vs generation manifest

`image/arch/packages.base` is a human-reviewed selection of package names. It
answers **what classes of packages should be present**, not **exactly what
generation was built**.

A real generation manifest must answer the latter and will record resolved
artifacts such as:

```text
distribution: arch
repository_context: <snapshot or acquisition identity>

packages:
  - name
    version
    architecture
    package_sha256

kernel:
  upstream_version
  source_identity
  config_sha256
  artifact_sha256

flowerly:
  source_commit
  package_or_binary_sha256

parent_generation
build_toolchain
build_timestamp/source_date_epoch
```

The exact schema lands with the Generation milestone rather than being guessed
prematurely.

## Kernel

Arch's `linux` package is allowed as a bootstrap dependency for early VM
rootfs/image work. It does not supersede the upstream-kernel policy.

The intended progression is:

```text
early prototype:
Arch linux package

        ↓

Flowerly generation builder:
upstream kernel source
+ explicit Flowerly config
+ reproducible packaging

        ↓

candidate generation
```

Kernel patching remains exceptional and requires its own architecture decision.

## Stable World package operations

Pacman remains available as a normal Linux tool. The long-term rule is about
system state, not hiding the tool:

- querying packages is ordinary administration;
- package operations inside candidate roots are ordinary build operations;
- system-wide upgrades that alter Stable World should flow through generation
  construction, testing and promotion;
- rollback is a generation operation, not an attempt to reverse pacman
  transaction history.

## Current implementation status

Implemented now:

- accepted Arch userspace architecture decision;
- initial `image/arch/` package and mkosi inputs;
- repository validation for those inputs.

Not implemented yet:

- Flowerly package production;
- bootable disk/UKI/initramfs output;
- package snapshot/cache retention;
- exact generation-manifest schema;
- KVM image tests;
- generation activation and rollback against a real installed system.

Those remain roadmap items and must not be described as completed.
