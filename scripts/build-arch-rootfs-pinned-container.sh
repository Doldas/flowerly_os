#!/usr/bin/env bash
# Build the Arch rootfs from the frozen snapshot with the BUILD TOOLING pinned
# too (#49, ADR-0020): the container image is named by digest and its pacman
# is pointed at the same dated Arch Archive snapshot as the rootfs, so mkosi,
# arch-install-scripts and friends are the snapshot's versions, not whatever
# the live mirrors serve today. The tool versions actually used are written
# next to the rootfs. Verify the result with:
#   distro-builder manifest verify-rootfs distro/base-image/arch/inputs.manifest.json --rootfs OUT/flowerly-rootfs
#
# usage: build-arch-rootfs-pinned-container.sh OUTPUT_DIR   (must not exist)
# Needs podman and network access to archive.archlinux.org and the registry.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG="$ROOT/distro/base-image/arch"
fail() { echo "pinned container build: $*" >&2; exit 1; }
(( $# == 1 )) || fail "usage: $0 OUTPUT_DIR"
OUT="$1"
[[ ! -e "$OUT" ]] || fail "refusing to reuse $OUT"
command -v podman >/dev/null || fail "podman is required"
read -r IMAGE TOOLS < <(python3 - "$CONFIG/build-tooling.json" <<'PY'
import json, re, sys
t = json.load(open(sys.argv[1]))
assert t["schema"] == "flowerly.build-tooling.v1"
assert re.fullmatch(r"[a-z0-9./_-]+@sha256:[0-9a-f]{64}", t["container_image"]), "image must be pinned by digest"
assert all(re.fullmatch(r"[a-z0-9@._+-]+", p) for p in t["tool_packages"])
print(t["container_image"], ",".join(t["tool_packages"]))
PY
)
SNAPSHOT="$(python3 -c 'import json,re,sys; u=json.load(open(sys.argv[1]))["source"]["snapshot_url"].rstrip("/"); assert re.fullmatch(r"https://archive\.archlinux\.org/repos/\d{4}/\d{2}/\d{2}", u), u; print(u)' "$CONFIG/inputs.manifest.json")"
mkdir -p "$OUT"; OUT="$(realpath "$OUT")"
podman run --rm --privileged --security-opt label=disable \
    -e SNAPSHOT="$SNAPSHOT" -e TOOLS="$TOOLS" \
    -v "$ROOT":/work:ro -v "$OUT":/out -w /work "$IMAGE" bash -euc '
        printf "Server = %s/\$repo/os/\$arch\n" "$SNAPSHOT" > /etc/pacman.d/mirrorlist
        pacman-key --init && pacman-key --populate archlinux
        # shellcheck disable=SC2086
        pacman -Syu --noconfirm --needed ${TOOLS//,/ }
        pacman -Q ${TOOLS//,/ } pacman > /out/tooling-versions.txt
        mkosi --directory=distro/base-image/arch --output-directory=/out \
            --mirror="$SNAPSHOT" build
    '
echo "Pinned build done: $OUT/flowerly-rootfs (tool versions: $OUT/tooling-versions.txt)"
