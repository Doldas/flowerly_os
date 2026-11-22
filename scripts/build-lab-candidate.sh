#!/usr/bin/env bash
# Build an Evolution Lab candidate (ADR-0024, #412) from a rootfs: two slot
# images, A (lab-a, the baseline) and B (lab-b, the candidate), both A/B test
# images with the guest probe, the two-slot disk with A active and B staged,
# and candidate.json, which binds the build to what each slot's probe will
# report. lab-candidate.py prepare arms a copy of the disk per run.
#
# Test profile only. Both slots get the probe because a rollback run falls
# back to A, and A must report too. The initramfs is an input (a generic mkosi
# initrd). The lane (FLOWERLY_LAB_LANE, ADR-0024 D4) says what the rootfs is:
#   reproducibility  (default) the PINNED rootfs. Both slots are prepared with
#                    FLOWERLY_FROZEN_INPUTS, so it must contain exactly the
#                    committed manifest's packages; the identities name that
#                    manifest and the plan claims its package set.
#   upstream         a rootfs resolved from the live mirror. Its package list
#                    (lab-probe.py --package-list) is resolved-packages.txt;
#                    the identities name that list's SHA-256 and the plan
#                    records the guest's package set without claiming it.
#
# usage: build-lab-candidate.sh ROOTFS INITRAMFS OUTPUT_DIR
# Run from a clean checkout; needs podman (user namespaces; not as root),
# e2fsprogs, dosfstools, sfdisk, mtools and systemd-sysusers on PATH, and
# release binaries in target/release (flowerlyd, agenticctl, evolutiond,
# distro-builder).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fail() { echo "lab candidate: $*" >&2; exit 1; }
(( $# == 3 )) || fail "usage: $0 ROOTFS INITRAMFS OUTPUT_DIR"
LANE="${FLOWERLY_LAB_LANE:-reproducibility}"
case "$LANE" in reproducibility|upstream) ;; *) fail "unknown lane $LANE" ;; esac
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
mkdir -p "$OUT"; OUT="$(realpath "$OUT")"
WORK="$OUT/work"; mkdir -p "$WORK"
# The rootfs and the slot copies are owned by the user namespace's
# subordinate ids, or outright by root (the CI runner's jobs run as root).
owner() { if (( EUID == 0 )); then "$@"; else podman unshare "$@"; fi; }
trap 'owner rm -rf "$WORK"' EXIT
if [[ "$LANE" == reproducibility ]]; then
    INPUT_SHA="$(sha256sum "$MANIFEST" | cut -d' ' -f1)"
    FROZEN="$BIN/distro-builder"
else
    # What the mirror resolved, read the way the guest's probe reads it.
    owner python3 -I "$ROOT/testbed/evolution-world/lab-probe.py" \
        --package-list "$ROOTFS" > "$OUT/resolved-packages.txt"
    INPUT_SHA="$(sha256sum "$OUT/resolved-packages.txt" | cut -d' ' -f1)"
    FROZEN=""
fi

PA="$(cat /proc/sys/kernel/random/uuid)"; PB="$(cat /proc/sys/kernel/random/uuid)"
read -r SCHEMA MIN < <("$BIN/flowerlyd" --schema-window |
    python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["schema_version"], d["min_reader"])')
# Byte for byte the generation-root.json the rootfs script bakes (with its
# newline): the probe hashes the baked file and describe compares the two.
identity() { # slot partuuid build_id
    printf '{"schema":"flowerly-generation-root/v2","slot":"%s","root_partuuid":"%s","source_commit":"%s","input_manifest_sha256":"%s","build_id":"%s","schema_version":%s,"min_reader":%s}\n' \
        "$1" "$2" "$COMMIT" "$INPUT_SHA" "$3" "$SCHEMA" "$MIN"
}
identity root-a "$PA" lab-a > "$OUT/identity-a.json"
identity root-b "$PB" lab-b > "$OUT/identity-b.json"

# Each slot: a private copy of the rootfs inside the user namespace that owns
# it, Flowerly staged, baked (in the reproducibility lane with the frozen set
# as a precondition; an empty FLOWERLY_FROZEN_INPUTS asks for none), the probe
# added, its file facts and kernel release taken from the finished tree, then
# the root volume.
slot() { # letter slot partuuid build_id
    owner env FLOWERLY_FROZEN_INPUTS="$FROZEN" bash -euc '
        cp -a "$1" "$2"
        bash "$3/scripts/install-flowerly-rootfs.sh" "$2" "$3/target/release"
        bash "$3/scripts/prepare-arch-boot.sh" "$2"
        bash "$3/scripts/prepare-generation-ab-rootfs.sh" "$2" --slot "$4" --partuuid "$5" \
            --source-commit "$6" --input-manifest-sha256 "$7" --build-id "$8" \
            --schema-version "$9" --min-reader "${10}" \
            --slot-broker "$3/target/release/distro-builder" --test-profile
        bash "$3/scripts/prepare-lab-probe-rootfs.sh" "$2" --test-profile
        python3 -I "$3/testbed/evolution-world/lab-probe.py" --offline "$2" > "${11}/facts-${12}.json"
        releases=("$2"/usr/lib/modules/*/vmlinuz)
        (( ${#releases[@]} == 1 )) && [[ -f "${releases[0]}" ]] || { echo "expected one kernel" >&2; exit 1; }
        basename "$(dirname "${releases[0]}")" > "${11}/kernel-release-${12}"
        FLOWERLY_ROOT_SIZE_MB="${13}" bash "$3/scripts/build-arch-root-volume.sh" "$2" "${11}/root-${12}.ext4"
    ' _ "$ROOTFS" "$WORK/rootfs-$1" "$ROOT" "$2" "$3" "$COMMIT" "$INPUT_SHA" "$4" \
        "$SCHEMA" "$MIN" "$OUT" "$1" "$SLOT_MB"
}
slot a root-a "$PA" lab-a
slot b root-b "$PB" lab-b

owner bash -euc '
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
python3 "$ROOT/scripts/lab-candidate.py" describe "$OUT" --lane "$LANE" \
    --source-commit "$COMMIT" --input-manifest-sha256 "$INPUT_SHA"
echo "BUILT $COMMIT lane=$LANE manifest=$INPUT_SHA out=$OUT"
