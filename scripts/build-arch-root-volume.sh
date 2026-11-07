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
# mkfs.ext4 and friends live in /usr/sbin, which unprivileged PATHs omit.
PATH="$PATH:/usr/sbin:/sbin"
for tool in mkfs.ext4 dumpe2fs truncate; do
    command -v "$tool" >/dev/null 2>&1 || fail "missing host tool: $tool"
done
# A rootfs built by unprivileged mkosi or podman is owned by subordinate ids
# on the host, so root-owned, restrictive files (for example the setuid
# dbus-daemon-launch-helper) are unreadable here. Never chmod or chown the
# tree and never run the whole pipeline as root: enter the user namespace
# that owns the tree instead, where those ids are the real ones again.
RUN=()
if [[ "$(id -u)" != 0 ]] &&
   [[ -n "$(find "$ROOTFS" 2>&1 >/dev/null | head -n1)" ||
      -n "$(find "$ROOTFS" ! -readable -print -quit 2>/dev/null)" ]]; then
    command -v podman >/dev/null 2>&1 && podman unshare true >/dev/null 2>&1 ||
        fail "rootfs has files unreadable by uid $(id -u) and no usable 'podman unshare'; build it in a user namespace you can enter, or as root in a disposable guest"
    RUN=(podman unshare)
    echo "root volume: rootfs has files owned by subordinate ids; running inside 'podman unshare'" >&2
fi
# Ensure the volume can hold staged data with room for filesystem metadata.
USED_KB="$("${RUN[@]}" du -sk "$ROOTFS" | cut -f1)"
(( USED_KB < SIZE_MB * 768 )) || fail "rootfs too large for requested volume"
mkdir -p "$(dirname "$IMAGE")"
TEMP="$(mktemp "$(dirname "$IMAGE")/.flowerly-root.XXXXXXXX")"
trap 'rm -f "$TEMP"' EXIT
truncate -s "${SIZE_MB}M" "$TEMP"
"${RUN[@]}" mkfs.ext4 -q -F -L FLOWERLY_ROOT -d "$ROOTFS" "$TEMP"
dumpe2fs -h "$TEMP" 2>/dev/null | grep -qE '^Filesystem volume name:[[:space:]]+FLOWERLY_ROOT$' ||
    fail "ext4 filesystem label verification failed"
# Prove owner, group and mode of every setuid/setgid entry survived.
if command -v debugfs >/dev/null 2>&1; then
    "${RUN[@]}" find "$ROOTFS" -xdev \( -type f -o -type d \) \( -perm -4000 -o -perm -2000 \) \
        -printf '%U:%G:%m:/%P\0' |
        python3 "$ROOT/scripts/audit-ext4-ownership.py" "$TEMP" ||
        fail "ownership audit of special-mode files failed"
else
    echo "root volume: debugfs missing; ownership audit skipped" >&2
fi
ln "$TEMP" "$IMAGE" || fail "output appeared during build; refusing overwrite"
rm -f "$TEMP"
trap - EXIT
echo "Verified populated ext4 root volume: $IMAGE"
echo "Not bootable: no partition table, bootloader or VM boot acceptance."
