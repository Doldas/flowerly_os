#!/usr/bin/env bash
# Build a verified Arch directory rootfs (not a bootable image).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG="$ROOT/distro/base-image/arch"
OUTPUT="$ROOT/target/flowerly-arch-rootfs"
ARTIFACT="$OUTPUT/flowerly-rootfs"

bash "$ROOT/scripts/check-image-inputs.sh"
for tool in mkosi pacman; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "Arch rootfs build requires $tool on the build host" >&2
        exit 1
    fi
done
if [[ -e "$ARTIFACT" ]]; then
    echo "rootfs already exists: $ARTIFACT; remove it before rebuilding" >&2
    exit 1
fi
mkdir -p "$OUTPUT"
# FLOWERLY_PINNED=1 builds from the frozen snapshot instead of a rolling
# mirror (#49, ADR-0020): the mirror is the manifest's own dated snapshot.
MIRROR_ARGS=()
if [[ "${FLOWERLY_PINNED:-0}" == "1" ]]; then
    SNAPSHOT="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["source"]["snapshot_url"].rstrip("/"))' "$CONFIG/inputs.manifest.json")"
    [[ "$SNAPSHOT" =~ ^https://archive\.archlinux\.org/repos/[0-9]{4}/[0-9]{2}/[0-9]{2}$ ]] ||
        { echo "manifest snapshot_url is not a dated Arch Archive snapshot" >&2; exit 1; }
    MIRROR_ARGS=(--mirror="$SNAPSHOT")
    echo "Pinned build from $SNAPSHOT; verify with: distro-builder manifest verify-rootfs"
fi
mkosi --directory="$CONFIG" --output-directory="$OUTPUT" "${MIRROR_ARGS[@]}" build
if [[ ! -d "$ARTIFACT" ]] || [[ ! -f "$ARTIFACT/usr/lib/os-release" ]] ||
   [[ ! -e "$ARTIFACT/usr/lib/systemd/systemd" ]] ||
   [[ ! -e "$ARTIFACT/usr/bin/pacman" ]]; then
    echo "mkosi did not produce a valid Arch userspace at $ARTIFACT" >&2
    exit 1
fi
grep -Eq '^ID=arch$' "$ARTIFACT/usr/lib/os-release" || {
    echo "rootfs os-release does not identify Arch Linux" >&2
    exit 1
}
echo "Verified Arch rootfs: $ARTIFACT"
echo "Not bootable; Flowerly services are not yet installed."
