#!/usr/bin/env bash
# Build the Gate D (ADR-0023 G5) KVM test inputs from the PINNED rootfs (#49):
# three slot images (A, B, and B2 with the test-only g5-schema-drill
# flowerlyd), their baked identities, the two-slot disk with B staged, and the
# kernel and boot loader taken from the frozen packages.
#
# Test profile only. Every slot is prepared with FLOWERLY_FROZEN_INPUTS, so the
# identity must name the committed manifest and the rootfs must contain exactly
# its packages, or nothing is written. The initramfs is an input (a generic
# mkosi initrd, not part of the frozen package set).
#
# usage: build-g5-test-image.sh PINNED_ROOTFS INITRAMFS OUTPUT_DIR
# Run from a clean checkout; needs podman (user namespaces), e2fsprogs,
# dosfstools, sfdisk and mtools on PATH, and release binaries in target/release
# (flowerlyd, agenticctl, evolutiond, distro-builder).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fail() { echo "g5 test image: $*" >&2; exit 1; }
(( $# == 3 )) || fail "usage: $0 PINNED_ROOTFS INITRAMFS OUTPUT_DIR"
ROOTFS="$(realpath "$1")"; INITRAMFS="$(realpath "$2")"; OUT="$3"
BIN="$ROOT/target/release"; MANIFEST="$ROOT/distro/base-image/arch/inputs.manifest.json"
SLOT_MB="${FLOWERLY_ROOT_SIZE_MB:-3072}"
[[ -d "$ROOTFS" && -f "$INITRAMFS" ]] || fail "missing rootfs or initramfs"
[[ ! -e "$OUT" ]] || fail "refusing to reuse $OUT"
for b in flowerlyd agenticctl evolutiond distro-builder; do
    [[ -x "$BIN/$b" ]] || fail "missing release binary $BIN/$b"
done
PATH="$PATH:/usr/sbin:/sbin"
COMMIT="$(git -C "$ROOT" rev-parse HEAD)"
[[ -z "$(git -C "$ROOT" status --porcelain --untracked-files=no)" ]] || fail "checkout is not clean"
INPUT_SHA="$(sha256sum "$MANIFEST" | cut -d' ' -f1)"
mkdir -p "$OUT"; OUT="$(realpath "$OUT")"
WORK="$OUT/work"; mkdir -p "$WORK"

# Slot B2's flowerlyd carries the test-only schema-bump migration; it is built
# into B2 only and never into a release build.
B2BIN="$OUT/bin-b2"; mkdir -p "$B2BIN"
CARGO_TARGET_DIR="$ROOT/target/g5-b2" cargo build --release --locked -p flowerlyd \
    --features g5-schema-drill
cp "$ROOT/target/g5-b2/release/flowerlyd" "$B2BIN/flowerlyd"
cp "$BIN/agenticctl" "$BIN/evolutiond" "$B2BIN/"

PA="$(cat /proc/sys/kernel/random/uuid)"; PB="$(cat /proc/sys/kernel/random/uuid)"
echo "$PA $PB" > "$OUT/uuids"
window() { "$1" --schema-window | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["schema_version"], d["min_reader"])'; }
read -r SA MA < <(window "$BIN/flowerlyd"); read -r S2 M2 < <(window "$B2BIN/flowerlyd")
identity() { # slot partuuid build_id schema min_reader
    printf '{"schema":"flowerly-generation-root/v2","slot":"%s","root_partuuid":"%s","source_commit":"%s","input_manifest_sha256":"%s","build_id":"%s","schema_version":%s,"min_reader":%s}' \
        "$1" "$2" "$COMMIT" "$INPUT_SHA" "$3" "$4" "$5"
}
identity root-a "$PA" g5-a "$SA" "$MA" > "$OUT/identity-a.json"
identity root-b "$PB" g5-b "$SA" "$MA" > "$OUT/identity-b.json"
identity root-b "$PB" g5-b2 "$S2" "$M2" > "$OUT/identity-b2.json"

# Each slot: a private copy of the pinned rootfs inside the user namespace that
# owns it, Flowerly staged, then baked with the frozen set as a precondition.
slot() { # name slot partuuid bindir identity_build schema min
    local name="$1" slotname="$2" uuid="$3" bindir="$4" build="$5" schema="$6" min="$7"
    podman unshare bash -euc '
        cp -a "$1" "$2"
        bash "$3/scripts/install-flowerly-rootfs.sh" "$2" "$4"
        bash "$3/scripts/prepare-arch-boot.sh" "$2"
        FLOWERLY_FROZEN_INPUTS="$3/target/release/distro-builder" \
        bash "$3/scripts/prepare-generation-ab-rootfs.sh" "$2" --slot "$5" --partuuid "$6" \
            --source-commit "$7" --input-manifest-sha256 "$8" --build-id "$9" \
            --schema-version "${10}" --min-reader "${11}" \
            --slot-broker "$3/target/release/distro-builder" --test-profile
        FLOWERLY_ROOT_SIZE_MB="${12}" bash "$3/scripts/build-arch-root-volume.sh" "$2" "${13}"
    ' _ "$ROOTFS" "$WORK/rootfs-$name" "$ROOT" "$bindir" "$slotname" "$uuid" "$COMMIT" \
        "$INPUT_SHA" "$build" "$schema" "$min" "$SLOT_MB" "$OUT/root-$name.ext4"
}
slot a root-a "$PA" "$BIN" g5-a "$SA" "$MA"
slot b root-b "$PB" "$BIN" g5-b "$SA" "$MA"
slot b2 root-b "$PB" "$B2BIN" g5-b2 "$S2" "$M2"

podman unshare bash -euc '
    cp "$1"/usr/lib/modules/*/vmlinuz "$2/vmlinuz"
    cp "$1/usr/lib/systemd/boot/efi/systemd-bootx64.efi" "$2/systemd-bootx64.efi"
' _ "$ROOTFS" "$OUT"
cp "$INITRAMFS" "$OUT/initramfs.img"

FLOWERLY_AB_ROOT_A_PARTUUID="$PA" FLOWERLY_AB_ROOT_B_PARTUUID="$PB" \
    FLOWERLY_BUILD_COMMIT="$COMMIT" \
    bash "$ROOT/scripts/build-flowerly-ab-disk.sh" --volume "$OUT/root-a.ext4" \
    --kernel "$OUT/vmlinuz" --initramfs "$OUT/initramfs.img" \
    --boot-efi "$OUT/systemd-bootx64.efi" --output "$OUT/ab.img" \
    --identity "$OUT/identity-a.json"
cp "$BIN/distro-builder" "$OUT/slot-tool"
"$OUT/slot-tool" slot stage --disk "$OUT/ab.img" --manifest "$OUT/ab.img.manifest.json" \
    --active-partuuid "$PA" --slot root-b --candidate "$OUT/root-b.ext4" \
    --sha256 "$(sha256sum "$OUT/root-b.ext4" | cut -d' ' -f1)" \
    --kernel "$OUT/vmlinuz" --initramfs "$OUT/initramfs.img" --identity "$OUT/identity-b.json"
echo "BUILT $COMMIT manifest=$INPUT_SHA out=$OUT"
