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

## Host dependencies and staging

On an Arch Linux host, install `mkosi`, `pacman`,
`arch-install-scripts`, `python-pefile`, `e2fsprogs` and
`qemu-desktop` before building. The source build additionally needs
Rust (via `rustup`) and `just`. See the
[complete build guide](../../../web-docs/docs/build.md) for installation
commands, checks, and troubleshooting.

Run the stages one at a time, stopping immediately if one fails:

```sh
just build
just image-rootfs
bash scripts/install-flowerly-rootfs.sh
bash scripts/prepare-arch-boot.sh
bash scripts/build-arch-root-volume.sh
python3 scripts/probe-arch-qemu-boot.py
```

The rootfs builder produces a directory tree. The installation stage copies
all three Flowerly release binaries and existing systemd units; only
`flowerlyd` is enabled by boot preparation. The ext4 stage constructs an
unpartitioned filesystem, and the QEMU stage attempts a direct-kernel boot
using kernel and initramfs files from that rootfs. No stage proves an
independently bootable disk or production service health.

The rootfs may contain unreadable, security-sensitive package files.
A real build observed `mkfs.ext4 -d` failing with `Permission denied` on
`dbus-daemon-launch-helper`. This is **not** a successful ext4 build.
Do not recursively change rootfs permissions or silently run all stages as
root; an ownership-aware build fix and real acceptance are still needed.

The experimental `bash scripts/build-arch-initrd.sh` command builds a
separate generic mkosi cpio initrd and stages it under
`boot/initramfs-linux.img`. It must run before ext4 volume creation.
Kernel-specific module inclusion and QEMU boot remain unverified.

## Current scope

The mkosi configuration produces an Arch package substrate. Flowerly
service staging and QEMU probing are implemented as separate experimental
steps. A partitioned bootable disk/ISO, verified VM boot and generation
promotion remain unimplemented.
