#!/usr/bin/env bash
# Build a separate Arch initramfs using mkosi's documented MakeInitrd facility.
# This does not inject kernel-specific modules or claim a successful QEMU boot.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOTFS="${1:-$ROOT/target/flowerly-arch-rootfs/flowerly-rootfs}"
WORK="$ROOT/target/flowerly-arch-initrd"
CONFIG="$ROOT/distro/base-image/arch-initrd"
ARTIFACT="$WORK/flowerly-initrd.cpio.zst"
DEST="$ROOTFS/boot/initramfs-linux.img"
fail() { echo "initrd build: $*" >&2; exit 1; }
[[ -d "$ROOTFS" && ! -L "$ROOTFS" ]] || fail "missing or symlinked Arch rootfs"
[[ -f "$ROOTFS/usr/lib/os-release" ]] &&
    grep -Eq '^ID=arch$' "$ROOTFS/usr/lib/os-release" ||
    fail "rootfs is not Arch Linux"
[[ ! -e "$DEST" && ! -L "$DEST" ]] || fail "initramfs already exists; refusing overwrite"
[[ ! -L "$ROOTFS/boot" ]] || fail "rootfs /boot is a symlink"
command -v mkosi >/dev/null 2>&1 || fail "mkosi required on host"
[[ ! -e "$ARTIFACT" && ! -L "$ARTIFACT" ]] ||
    fail "initrd output already exists; refusing overwrite"
mkdir -p "$WORK"
mkosi --directory="$CONFIG" --output-directory="$WORK" build
[[ -f "$ARTIFACT" && ! -L "$ARTIFACT" && -s "$ARTIFACT" ]] ||
    fail "mkosi did not produce the expected compressed cpio initrd"
install -d -m 0755 "$ROOTFS/boot"
install -m 0644 "$ARTIFACT" "$DEST"
echo "Staged generic Arch initramfs: $DEST"
echo "Kernel-specific virtio/ext4 modules are not yet guaranteed; boot remains unverified."
