#!/usr/bin/env bash
# Prepare the staged Arch rootfs for a future boot test; does not create a bootable disk.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOTFS="${1:-$ROOT/target/flowerly-arch-rootfs/flowerly-rootfs}"

fail() { echo "boot preparation: $*" >&2; exit 1; }
[[ -d "$ROOTFS" && ! -L "$ROOTFS" ]] || fail "rootfs missing or symlinked"
[[ -f "$ROOTFS/usr/lib/os-release" ]] || fail "missing os-release"
grep -Eq '^ID=arch$' "$ROOTFS/usr/lib/os-release" || fail "not an Arch rootfs"
for path in usr/lib/systemd/systemd usr/bin/flowerlyd usr/bin/agenticctl             usr/lib/systemd/system/flowerlyd.service usr/lib/sysusers.d/flowerly.conf; do
    [[ -f "$ROOTFS/$path" && ! -L "$ROOTFS/$path" ]] || fail "missing or symlinked: $path"
done
[[ -x "$ROOTFS/usr/lib/systemd/systemd" && -x "$ROOTFS/usr/bin/flowerlyd" ]] ||
    fail "systemd and flowerlyd must be executable"
# systemd-sysusers must be available before the service starts as User=flowerly.
[[ -x "$ROOTFS/usr/bin/systemd-sysusers" ||
   -x "$ROOTFS/usr/lib/systemd/systemd-sysusers" ]] ||
    fail "systemd-sysusers missing from rootfs"
WITNESS="$ROOT/distro/overlay/usr/lib/systemd/system/flowerly-boot-witness.service"
[[ -f "$WITNESS" ]] || fail "missing Flowerly boot witness unit"
[[ ! -L "$ROOTFS/usr/lib/systemd/system" ]] || fail "systemd unit directory is symlinked"
UNITDIR="$ROOTFS/etc/systemd/system/multi-user.target.wants"
[[ ! -L "$ROOTFS/etc" && ! -L "$ROOTFS/etc/systemd" &&
   ! -L "$ROOTFS/etc/systemd/system" && ! -L "$UNITDIR" ]] ||
    fail "systemd unit destination contains a symlink"
[[ ! -e "$UNITDIR/flowerlyd.service" && ! -L "$UNITDIR/flowerlyd.service" ]] ||
    fail "flowerlyd already enabled or path occupied"
[[ ! -e "$UNITDIR/flowerly-boot-witness.service" && ! -L "$UNITDIR/flowerly-boot-witness.service" ]] ||
    fail "boot witness already enabled or path occupied"
[[ ! -e "$UNITDIR/evolutiond.service" && ! -L "$UNITDIR/evolutiond.service" ]] ||
    fail "evolutiond must stay disabled"
install -d -m 0755 "$UNITDIR"
install -m 0644 "$WITNESS" "$ROOTFS/usr/lib/systemd/system/flowerly-boot-witness.service"
ln -s /usr/lib/systemd/system/flowerlyd.service "$UNITDIR/flowerlyd.service"
ln -s /usr/lib/systemd/system/flowerly-boot-witness.service "$UNITDIR/flowerly-boot-witness.service"
echo "Prepared systemd enablement of flowerlyd in Arch rootfs: $ROOTFS"
echo "Not bootable: disk partitioning, initramfs, bootloader and VM boot still pending."
