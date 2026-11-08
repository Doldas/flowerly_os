#!/usr/bin/env bash
# Assemble the prototype two-slot Flowerly disk (ADR-0019): GPT with an ESP
# holding systemd-boot and a boot-counting entry for slot A, two root slots
# and a separate data partition. Builds and verifies the image only; it does
# not boot it and does not claim A/B rollback works.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ESP_MB="${FLOWERLY_ESP_MB:-256}"
DATA_MB="${FLOWERLY_DATA_MB:-512}"
TRIES="${FLOWERLY_BOOT_TRIES:-3}"
COMMIT="${FLOWERLY_BUILD_COMMIT:-$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo unknown)}"
fail() { echo "ab disk: $*" >&2; exit 1; }
usage() {
    echo "usage: $0 --volume ROOT.ext4 --kernel VMLINUZ --initramfs INITRAMFS --boot-efi systemd-bootx64.efi --output DISK.img [--manifest FILE]" >&2
    exit 2
}

VOLUME= KERNEL= INITRAMFS= BOOTEFI= OUTPUT= MANIFEST=
while (( $# )); do
    (( $# >= 2 )) || usage
    case "$1" in
        --volume) VOLUME="$2" ;;
        --kernel) KERNEL="$2" ;;
        --initramfs) INITRAMFS="$2" ;;
        --boot-efi) BOOTEFI="$2" ;;
        --output) OUTPUT="$2" ;;
        --manifest) MANIFEST="$2" ;;
        *) usage ;;
    esac
    shift 2
done
[[ -n "$VOLUME" && -n "$KERNEL" && -n "$INITRAMFS" && -n "$BOOTEFI" && -n "$OUTPUT" ]] || usage
MANIFEST="${MANIFEST:-$OUTPUT.manifest.json}"

[[ "$ESP_MB" =~ ^[1-9][0-9]*$ ]] && (( ESP_MB >= 64 && ESP_MB <= 1024 )) ||
    fail "FLOWERLY_ESP_MB must be 64..1024"
[[ "$DATA_MB" =~ ^[1-9][0-9]*$ ]] && (( DATA_MB >= 64 && DATA_MB <= 65536 )) ||
    fail "FLOWERLY_DATA_MB must be 64..65536"
[[ "$TRIES" =~ ^[1-9][0-9]?$ ]] || fail "FLOWERLY_BOOT_TRIES must be 1..99"
for input in "$VOLUME" "$KERNEL" "$INITRAMFS" "$BOOTEFI"; do
    [[ -f "$input" && ! -L "$input" && -s "$input" ]] ||
        fail "input missing, empty or symlinked: $input"
done
for output in "$OUTPUT" "$MANIFEST"; do
    [[ ! -e "$output" && ! -L "$output" ]] || fail "refusing to overwrite output: $output"
    [[ ! -L "$(dirname "$output")" ]] || fail "output directory is a symlink: $output"
done
[[ "$(realpath -m "$OUTPUT")" != "$(realpath -m "$MANIFEST")" ]] || fail "disk and manifest paths must differ"
[[ "$(head -c2 "$BOOTEFI")" == MZ ]] || fail "boot loader is not a PE/EFI binary"

PATH="$PATH:/usr/sbin:/sbin"
for tool in sfdisk mkfs.vfat mkfs.ext4 dumpe2fs mmd mcopy truncate dd sha256sum python3; do
    command -v "$tool" >/dev/null 2>&1 || fail "missing host tool: $tool"
done
VOLUME_BYTES="$(stat -c %s "$VOLUME")"
(( VOLUME_BYTES % 1048576 == 0 )) || fail "root volume size must be a whole number of MiB"
dumpe2fs -h "$VOLUME" 2>/dev/null |
    grep -qE '^Filesystem volume name:[[:space:]]+FLOWERLY_ROOT$' ||
    fail "root volume is not an ext4 filesystem labelled FLOWERLY_ROOT"
ROOT_MB=$(( VOLUME_BYTES / 1048576 ))

# 1 MiB alignment gap in front, 1 MiB left for the backup GPT.
ESP_START=1
ROOT_A_START=$(( ESP_START + ESP_MB ))
ROOT_B_START=$(( ROOT_A_START + ROOT_MB ))
DATA_START=$(( ROOT_B_START + ROOT_MB ))
TOTAL_MB=$(( DATA_START + DATA_MB + 1 ))

uuid() { cat /proc/sys/kernel/random/uuid; }
DISK_UUID="$(uuid)"; ESP_UUID="$(uuid)"; DATA_UUID="$(uuid)"
ROOT_A_UUID="${FLOWERLY_AB_ROOT_A_PARTUUID:-$(uuid)}"
ROOT_B_UUID="${FLOWERLY_AB_ROOT_B_PARTUUID:-$(uuid)}"
# Root identity is baked into the image BEFORE GPT assembly. Preserve
# canonical user-selected UUIDs instead of silently generating new ones.
uuid_re="^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$"
[[ "$ROOT_A_UUID" =~ $uuid_re && "$ROOT_B_UUID" =~ $uuid_re ]] ||
    fail "root slot PARTUUID must be a lowercase canonical UUID"
[[ "$ROOT_A_UUID" != "$ROOT_B_UUID" && "$ROOT_A_UUID" != "$ESP_UUID" &&
   "$ROOT_B_UUID" != "$ESP_UUID" && "$ROOT_A_UUID" != "$DATA_UUID" &&
   "$ROOT_B_UUID" != "$DATA_UUID" ]] ||
    fail "root slot PARTUUIDs must be distinct"

WORK="$(mktemp -d "$(dirname "$OUTPUT")/.flowerly-ab.XXXXXXXX")"
trap 'rm -rf "$WORK"' EXIT
DISK="$WORK/disk.img"
TEMP_MANIFEST="$WORK/manifest.json"

# ESP contents are written with mtools so no mount or root is needed.
ESP="$WORK/esp.img"
truncate -s "${ESP_MB}M" "$ESP"
mkfs.vfat -F 32 -n FLOWERLYESP "$ESP" >/dev/null
mmd -i "$ESP" ::/EFI ::/EFI/systemd ::/EFI/BOOT ::/loader ::/loader/entries ::/flowerly ::/flowerly/a
mcopy -i "$ESP" "$BOOTEFI" ::/EFI/systemd/systemd-bootx64.efi
mcopy -i "$ESP" "$BOOTEFI" ::/EFI/BOOT/BOOTX64.EFI
mcopy -i "$ESP" "$KERNEL" ::/flowerly/a/vmlinuz
mcopy -i "$ESP" "$INITRAMFS" ::/flowerly/a/initramfs.img
printf 'default flowerly-a*\ntimeout 0\nconsole-mode keep\n' > "$WORK/loader.conf"
ENTRY="flowerly-a+${TRIES}.conf"
cat > "$WORK/$ENTRY" <<ENTRY_EOF
title Flowerly OS slot A
linux /flowerly/a/vmlinuz
initrd /flowerly/a/initramfs.img
options root=PARTUUID=${ROOT_A_UUID} ro rootfstype=ext4 systemd.volatile=state fsck.mode=skip console=ttyS0 systemd.unit=multi-user.target loglevel=6
ENTRY_EOF
mcopy -i "$ESP" "$WORK/loader.conf" ::/loader/loader.conf
mcopy -i "$ESP" "$WORK/$ENTRY" "::/loader/entries/$ENTRY"

DATA="$WORK/data.img"
truncate -s "${DATA_MB}M" "$DATA"
mkfs.ext4 -q -F -L FLOWERLY_DATA -U "$DATA_UUID" "$DATA"

truncate -s "${TOTAL_MB}M" "$DISK"
sfdisk --quiet "$DISK" <<SFDISK_EOF
label: gpt
label-id: ${DISK_UUID}
unit: sectors
first-lba: 2048

start=$(( ESP_START * 2048 )), size=$(( ESP_MB * 2048 )), type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, uuid=${ESP_UUID}, name="esp"
start=$(( ROOT_A_START * 2048 )), size=$(( ROOT_MB * 2048 )), type=4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709, uuid=${ROOT_A_UUID}, name="root-a"
start=$(( ROOT_B_START * 2048 )), size=$(( ROOT_MB * 2048 )), type=4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709, uuid=${ROOT_B_UUID}, name="root-b"
start=$(( DATA_START * 2048 )), size=$(( DATA_MB * 2048 )), type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, uuid=${DATA_UUID}, name="data"
SFDISK_EOF

dd if="$ESP" of="$DISK" bs=1M seek="$ESP_START" conv=notrunc status=none
dd if="$VOLUME" of="$DISK" bs=1M seek="$ROOT_A_START" conv=notrunc status=none
dd if="$DATA" of="$DISK" bs=1M seek="$DATA_START" conv=notrunc status=none

slice_sha() { dd if="$DISK" bs=1M skip="$1" count="$2" status=none | sha256sum | cut -d' ' -f1; }
sha() { sha256sum "$1" | cut -d' ' -f1; }
# Read the partitions back from the finished disk; any mismatch fails closed.
[[ "$(slice_sha "$ESP_START" "$ESP_MB")" == "$(sha "$ESP")" ]] || fail "ESP readback hash mismatch"
[[ "$(slice_sha "$ROOT_A_START" "$ROOT_MB")" == "$(sha "$VOLUME")" ]] || fail "root-a readback hash mismatch"
[[ "$(slice_sha "$DATA_START" "$DATA_MB")" == "$(sha "$DATA")" ]] || fail "data readback hash mismatch"
ROOT_B_SHA="$(slice_sha "$ROOT_B_START" "$ROOT_MB")"
[[ "$ROOT_B_SHA" == "$(head -c $(( ROOT_MB * 1048576 )) /dev/zero | sha256sum | cut -d' ' -f1)" ]] ||
    fail "root-b is not empty"

export DISK_UUID ESP_UUID ROOT_A_UUID ROOT_B_UUID DATA_UUID COMMIT ENTRY ROOT_MB ESP_MB DATA_MB \
    ESP_START ROOT_A_START ROOT_B_START DATA_START TOTAL_MB
export ESP_SHA="$(slice_sha "$ESP_START" "$ESP_MB")" ROOT_A_SHA="$(slice_sha "$ROOT_A_START" "$ROOT_MB")" \
    ROOT_B_SHA DATA_SHA="$(slice_sha "$DATA_START" "$DATA_MB")" DISK_SHA="$(sha "$DISK")" \
    BOOTEFI_SHA="$(sha "$BOOTEFI")" KERNEL_SHA="$(sha "$KERNEL")" INITRAMFS_SHA="$(sha "$INITRAMFS")" \
    LOADER_SHA="$(sha "$WORK/loader.conf")" ENTRY_SHA="$(sha "$WORK/$ENTRY")"
python3 - "$TEMP_MANIFEST" <<'PY_EOF'
import json, os, sys
e = os.environ
MIB = 1048576
def part(name, label, uuid, start, size, sha):
    return {"name": name, "filesystem": label, "partuuid": uuid,
            "offset_bytes": int(start) * MIB, "size_bytes": int(size) * MIB, "sha256": sha}
manifest = {
    "schema": "flowerly-ab-disk/v1",
    "builder_commit": e["COMMIT"],
    "disk": {"guid": e["DISK_UUID"], "size_bytes": int(e["TOTAL_MB"]) * MIB, "sha256": e["DISK_SHA"]},
    "partitions": [
        part("esp", "vfat", e["ESP_UUID"], e["ESP_START"], e["ESP_MB"], e["ESP_SHA"]),
        part("root-a", "ext4", e["ROOT_A_UUID"], e["ROOT_A_START"], e["ROOT_MB"], e["ROOT_A_SHA"]),
        part("root-b", "empty", e["ROOT_B_UUID"], e["ROOT_B_START"], e["ROOT_MB"], e["ROOT_B_SHA"]),
        part("data", "ext4", e["DATA_UUID"], e["DATA_START"], e["DATA_MB"], e["DATA_SHA"]),
    ],
    "esp_files": {
        "/EFI/systemd/systemd-bootx64.efi": e["BOOTEFI_SHA"],
        "/EFI/BOOT/BOOTX64.EFI": e["BOOTEFI_SHA"],
        "/flowerly/a/vmlinuz": e["KERNEL_SHA"],
        "/flowerly/a/initramfs.img": e["INITRAMFS_SHA"],
        "/loader/loader.conf": e["LOADER_SHA"],
        "/loader/entries/" + e["ENTRY"]: e["ENTRY_SHA"],
    },
}
with open(sys.argv[1], "w") as handle:
    json.dump(manifest, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY_EOF

ln "$DISK" "$OUTPUT" || fail "output appeared during build; refusing overwrite"
(set -o noclobber; cat "$TEMP_MANIFEST" > "$MANIFEST") 2>/dev/null ||
    { rm -f "$OUTPUT"; fail "manifest appeared during build; refusing overwrite"; }
echo "Built two-slot Flowerly disk: $OUTPUT"
echo "Manifest: $MANIFEST"
echo "Not verified: firmware boot (OVMF), boot counting and slot fallback are untested."
