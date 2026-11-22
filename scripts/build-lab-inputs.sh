#!/usr/bin/env bash
# Build the Evolution Lab's inputs for one lane (ADR-0024 D4, #414): the Arch
# rootfs and the generic mkosi initrd, in one run of the digest-pinned tooling
# container (distro/base-image/arch/build-tooling.json).
#
#   reproducibility  tooling and both images from the committed manifest's
#                    dated Arch Archive snapshot, as
#                    build-arch-rootfs-pinned-container.sh does: the rootfs
#                    the candidate build's frozen-set check accepts.
#   upstream         tooling and both images from the live Arch mirror: what
#                    Arch serves today. The candidate build records the
#                    package set it resolved instead of checking it, and an
#                    upstream run is never reproducibility evidence.
#
# Writes OUTPUT_DIR/flowerly-rootfs, OUTPUT_DIR/initramfs.img and
# OUTPUT_DIR/tooling-versions.txt, and for upstream OUTPUT_DIR/resolved-at.txt
# (when and from where). Feed them to vm-test.sh --lane LANE.
#
# usage: build-lab-inputs.sh reproducibility|upstream OUTPUT_DIR   (must not exist)
# Needs podman and network access to the registry and the lane's mirror.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG="$ROOT/distro/base-image/arch"
UPSTREAM_MIRROR="https://geo.mirror.pkgbuild.com"
fail() { echo "lab inputs: $*" >&2; exit 1; }
usage() { echo "usage: $0 reproducibility|upstream OUTPUT_DIR" >&2; exit 2; }
(( $# == 2 )) || usage
LANE="$1"; OUT="$2"
[[ "$LANE" == reproducibility || "$LANE" == upstream ]] || usage
[[ ! -e "$OUT" && ! -L "$OUT" ]] || fail "refusing to reuse $OUT"
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
if [[ "$LANE" == reproducibility ]]; then
    MIRROR="$(python3 -c 'import json,re,sys; u=json.load(open(sys.argv[1]))["source"]["snapshot_url"].rstrip("/"); assert re.fullmatch(r"https://archive\.archlinux\.org/repos/\d{4}/\d{2}/\d{2}", u), u; print(u)' "$CONFIG/inputs.manifest.json")"
else
    MIRROR="$UPSTREAM_MIRROR"
fi
mkdir -p "$OUT"; OUT="$(realpath "$OUT")"
if [[ "$LANE" == upstream ]]; then
    printf 'resolved_at=%s\nmirror=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$MIRROR" > "$OUT/resolved-at.txt"
fi
# --network host: on a runner that is itself a container, nested podman's
# bridge network cannot reach DNS (pacman times out resolving the mirror).
podman run --rm --privileged --security-opt label=disable --network host \
    -e MIRROR="$MIRROR" -e TOOLS="$TOOLS" \
    -v "$ROOT":/work:ro -v "$OUT":/out -w /work "$IMAGE" bash -euc '
        printf "Server = %s/\$repo/os/\$arch\n" "$MIRROR" > /etc/pacman.d/mirrorlist
        pacman-key --init && pacman-key --populate archlinux
        # shellcheck disable=SC2086
        pacman -Syu --noconfirm --needed ${TOOLS//,/ }
        pacman -Q ${TOOLS//,/ } pacman > /out/tooling-versions.txt
        mkosi --directory=distro/base-image/arch --output-directory=/out \
            --mirror="$MIRROR" build
        mkdir /out/initrd
        mkosi --directory=distro/base-image/arch-initrd --output-directory=/out/initrd \
            --mirror="$MIRROR" build
        initrd=/out/initrd/flowerly-initrd.cpio.zst
        [[ -f "$initrd" && ! -L "$initrd" && -s "$initrd" ]] || {
            echo "mkosi did not produce $initrd" >&2; exit 1; }
        install -m 0644 "$initrd" /out/initramfs.img
        rm -rf /out/initrd
    '
[[ -d "$OUT/flowerly-rootfs" && -s "$OUT/initramfs.img" ]] || fail "the build left no rootfs or initramfs"
echo "LAB INPUTS lane=$LANE mirror=$MIRROR out=$OUT"
