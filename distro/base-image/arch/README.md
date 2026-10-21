# Arch image foundation

This directory defines the initial Arch Linux userspace inputs for Flowerly OS.
It is intentionally small: this is the seed for the future image/generation
builder, not yet a release image.

Architecture policy is defined by
[ADR-0015](../../docs/adr/0015-arch-linux-userspace-substrate.md).

## Principle

Arch is an upstream package substrate. Flowerly owns promotion and rollback:

```text
Arch repositories
      ↓
candidate package resolution
      ↓
Flowerly candidate generation
      ↓
KVM / Evolution World
      ↓
promote or reject
      ↓
Stable World
```

Stable World should not eventually be maintained by ad-hoc system-wide
`pacman -Syu` operations. Full Arch upgrades are resolved into a candidate
root filesystem, recorded in a generation manifest, tested, and only then
promoted.

## Files

- `mkosi.conf` — initial declarative Arch rootfs prototype.
- `packages.base` — package names required by the seed userspace.
- `flowerly-binaries.txt` — Flowerly executables that a later packaging/image
  step must install.
- `../../scripts/check-image-inputs.sh` — cheap repository-level validation of
  these inputs.

The package-name list is **not** sufficient for reproducibility. The eventual
generation builder must capture exact resolved versions, architecture, package
hashes/repository provenance, kernel identity, Flowerly source commit and output
hashes.

Build a real Arch directory rootfs on a host with mkosi and pacman using\n`just image-rootfs`. This verifies Arch identity and core userspace files; it\ndoes not produce a bootable disk or install Flowerly binaries.\n\nUse `bash scripts/install-flowerly-rootfs.sh` (from the repository root) after
building the Arch rootfs and release binaries. This stages the three Flowerly
executables, existing systemd units, and sysusers declarations without enabling
services; an installed runnable OS still needs boot and service validation.

The next opt-in command, `bash scripts/prepare-arch-boot.sh`, validates
the staged supervisor, systemd and sysusers before enabling `flowerlyd`
for a future boot test. It does not enable `evolutiond` or make the
rootfs bootable.

`bash scripts/build-arch-root-volume.sh` can turn the staged Arch directory
into a populated ext4 root volume without requiring root privileges.
It does not create a partition table or boot loader, and does not imply a
successful virtual-machine boot.

## Current scope

The mkosi configuration intentionally builds only the Arch rootfs substrate.
Installing locally built Flowerly packages/services, producing a bootable disk,
creating initramfs/UKIs, signing, and KVM boot testing remain future work.

This staged approach keeps the repository honest: the direction is concrete
without pretending that the 0.0.9 bootable-image milestone is complete.
