#!/usr/bin/env bash
# Stage Flowerly services in an existing Arch rootfs; do not enable/start services.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOTFS="${1:-$ROOT/target/flowerly-arch-rootfs/flowerly-rootfs}"
BIN_DIR="${2:-$ROOT/target/release}"
LIST="$ROOT/distro/base-image/arch/flowerly-binaries.txt"
UNITS="$ROOT/distro/overlay/usr/lib/systemd/system"
# Opt-in multi-user drop-in (#225): staged as documentation, never activated.
MULTI_USER="$ROOT/distro/overlay/usr/share/flowerly/systemd/flowerlyd-multi-user.conf"

if [[ ! -f "$ROOTFS/usr/lib/os-release" ]] ||
   ! grep -Eq '^ID=arch$' "$ROOTFS/usr/lib/os-release"; then
    echo "refusing to stage binaries into a non-Arch or missing rootfs" >&2
    exit 1
fi
for binary in agenticctl evolutiond flowerlyd; do
    if ! grep -qx "$binary" "$LIST" || [[ ! -f "$BIN_DIR/$binary" ]] ||
       [[ ! -x "$BIN_DIR/$binary" ]]; then
        echo "missing executable Flowerly release binary: $BIN_DIR/$binary" >&2
        exit 1
    fi
done
for service in flowerlyd evolutiond; do
    if [[ ! -f "$UNITS/$service.service" ]]; then
        echo "missing Flowerly systemd unit: $service" >&2
        exit 1
    fi
done
for directory in usr/bin usr/lib/systemd/system usr/lib/sysusers.d usr/share/flowerly/systemd; do
    if [[ -L "$ROOTFS/$directory" ]]; then
        echo "refusing symlinked rootfs destination: $directory" >&2
        exit 1
    fi
done
install -d -m 0755 "$ROOTFS/usr/bin" "$ROOTFS/usr/lib/systemd/system" "$ROOTFS/usr/lib/sysusers.d" \
    "$ROOTFS/usr/share/flowerly/systemd"
for binary in agenticctl evolutiond flowerlyd; do
    install -m 0755 "$BIN_DIR/$binary" "$ROOTFS/usr/bin/$binary"
done
for service in flowerlyd evolutiond; do
    install -m 0644 "$UNITS/$service.service" "$ROOTFS/usr/lib/systemd/system/$service.service"
done
install -m 0644 "$ROOT/distro/overlay/usr/lib/sysusers.d/flowerly.conf" "$ROOTFS/usr/lib/sysusers.d/flowerly.conf"
install -m 0644 "$MULTI_USER" "$ROOTFS/usr/share/flowerly/systemd/flowerlyd-multi-user.conf"
echo "Flowerly binaries, systemd services and system users staged in $ROOTFS"
echo "No service has been enabled. Boot integration and release acceptance remain pending."
