# Arch Linux base-image inputs

The initial Arch package manifest and mkosi configuration live in
[`arch/`](arch/README.md). `just image-rootfs` builds a directory root
filesystem on a provisioned Arch Linux host. It requires mkosi, pacman,
user namespace support or another supported mkosi execution mode, trusted
package metadata, network access and free disk space.

The following *separate* scripts stage Flowerly release binaries, prepare
systemd, build an ext4 volume, and attempt a direct-kernel QEMU boot.
They are not a standalone bootable image pipeline. The ext4 builder
handles rootfs trees owned by subordinate ids by running `mkfs.ext4` inside
`podman unshare` and verifies setuid/setgid ownership afterwards.

See [Arch image setup](arch/README.md) and the
[build requirements and troubleshooting](../../web-docs/docs/build.md).
Upstream Arch content and Flowerly generations remain separate.
