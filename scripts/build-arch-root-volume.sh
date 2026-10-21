#!/usr/bin/env bash
# Populate an unpartitioned ext4 disk volume from the staged Flowerly rootfs.
# This volume is not, by itself, a bootable disk image.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOTFS="${1:-$ROOT/target/flowerly-arch-rootfs/flowerly-rootfs}"
IMAGE="${2:-$ROOT/target/flowerly-arch-rootfs/flowerly-root.ext4}"
SIZE_MB="${FLOWERLY_ROOT_SIZE_MB:-4096}"
fail() { echo "root volume: $*" >&2; exit 1; }

[[ "$SIZE_MB" =~ ^[1-9][0-9]*$ ]] && (( SIZE_MB >= 512 && SIZE_MB <= 65536 )) ||
    fail "FLOWERLY_ROOT_SIZE_MB must be 512..65536"
[[ -d "$ROOTFS" && ! -L "$ROOTFS" ]] || fail "rootfs missing or symlinked"
[[ -f "$ROOTFS/usr/lib/os-release" ]] &&
    grep -Eq '^ID=arch$' "$ROOTFS/usr/lib/os-release" || fail "not an Arch rootfs"
for file in usr/lib/systemd/systemd usr/bin/flowerlyd usr/lib/systemd/system/flowerlyd.service; do
    [[ -f "$ROOTFS/$file" ]] || fail "missing staged OS file: $file"
done
[[ ! -e "$IMAGE" && ! -L "$IMAGE" ]] || fail "refusing to overwrite output"
[[ ! -L "$(dirname "$IMAGE")" ]] || fail "output directory is a symlink"
for tool in mkfs.ext4 dumpe2fs truncate; do
    command -v "$tool" >/dev/null 2>&1 || fail "missing host tool: $tool"
done
# Ensure the volume can hold staged data with room for filesystem metadata.
USED_KB="$(du -sk "$ROOTFS" | cut -f1)"
(( USED_KB < SIZE_MB * 768 )) || fail "rootfs too large for requested volume"
mkdir -p "$(dirname "$IMAGE")"
TEMP="$(mktemp "$(dirname "$IMAGE")/.flowerly-root.XXXXXXXX")"
trap 'rm -f "$TEMP"' EXIT
truncate -s "${SIZE_MB}M" "$TEMP"
mkfs.ext4 -q -F -L FLOWERLY_ROOT -d "$ROOTFS" "$TEMP"
dumpe2fs -h "$TEMP" 2>/dev/null | grep -qE '^Filesystem volume name:[[:space:]]+FLOWERLY_ROOT$' ||
    fail "ext4 filesystem label verification failed"
mv -n "$TEMP" "$IMAGE"
trap - EXIT
echo "Verified populated ext4 root volume: $IMAGE"
echo "Not bootable: no partition table, bootloader or VM boot acceptance."
