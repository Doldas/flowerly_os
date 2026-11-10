#!/usr/bin/env bash
# Explicit opt-in: prepare an already staged Arch rootfs as an A/B KVM test
# image. Not used by shipped distro or the existing direct-kernel boot probe.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
fail() { echo "Generation A/B rootfs: $*" >&2; exit 1; }
(( $# == 18 )) || fail "usage: ROOTFS --slot root-a|root-b --partuuid UUID --source-commit SHA40 --input-manifest-sha256 SHA64 --build-id ID --schema-version N --min-reader N --slot-broker BINARY --test-profile"
ROOTFS="$1"; shift
[[ "$1" == --slot ]] || fail "missing --slot"; SLOT="$2"; shift 2
[[ "$1" == --partuuid ]] || fail "missing --partuuid"; PARTUUID="$2"; shift 2
[[ "$1" == --source-commit ]] || fail "missing --source-commit"; SOURCE="$2"; shift 2
[[ "$1" == --input-manifest-sha256 ]] || fail "missing --input-manifest-sha256"; INPUT_SHA="$2"; shift 2
[[ "$1" == --build-id ]] || fail "missing --build-id"; BUILD_ID="$2"; shift 2
# The schema window of this image's flowerlyd (flowerlyd --schema-window).
[[ "$1" == --schema-version ]] || fail "missing --schema-version"; SCHEMA_VERSION="$2"; shift 2
[[ "$1" == --min-reader ]] || fail "missing --min-reader"; MIN_READER="$2"; shift 2
[[ "$1" == --slot-broker ]] || fail "missing --slot-broker"; BROKER="$2"; shift 2
[[ "$1" == --test-profile ]] || fail "Generation A/B boot profile is test-only"
[[ "$SLOT" == root-a || "$SLOT" == root-b ]] || fail "invalid target slot"
[[ "$PARTUUID" =~ ^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$ ]] || fail "invalid root PARTUUID"
[[ "$SOURCE" =~ ^[0-9a-f]{40}$ && "$INPUT_SHA" =~ ^[0-9a-f]{64}$ ]] || fail "invalid source/input digest"
[[ "$BUILD_ID" =~ ^[A-Za-z0-9_.-]{1,64}$ ]] || fail "invalid build ID"
if [[ ! "$SCHEMA_VERSION" =~ ^[1-9][0-9]{0,5}$ || ! "$MIN_READER" =~ ^[1-9][0-9]{0,5}$ ]] ||
   (( MIN_READER > SCHEMA_VERSION )); then
    fail "invalid schema window"
fi
[[ -f "$BROKER" && ! -L "$BROKER" && -x "$BROKER" ]] || fail "missing slot broker binary"
[[ -d "$ROOTFS" && ! -L "$ROOTFS" ]] || fail "missing/symlinked rootfs"
[[ -f "$ROOTFS/usr/lib/os-release" ]] &&
    grep -qx ID=arch "$ROOTFS/usr/lib/os-release" || fail "not an Arch rootfs"
ROOTFS_REAL="$(realpath "$ROOTFS")"
for file in usr/bin/python3 usr/bin/runuser usr/lib/systemd/systemd-bless-boot \
            usr/lib/systemd/system/flowerlyd.service usr/lib/flowerly/boot-ipc-health.sh; do
    candidate="$ROOTFS/$file"
    if [[ -L "$candidate" ]]; then
        link="$(readlink "$candidate")"
        if [[ "$link" == /* ]]; then
            resolved="$(realpath -m "$ROOTFS$link")"
        else
            resolved="$(realpath -m "$(dirname "$candidate")/$link")"
        fi
        [[ "$resolved" == "$ROOTFS_REAL/"* ]] ||
            fail "A/B prerequisite symlink escapes image: $file"
        [[ -f "$resolved" ]] || fail "unresolved internal A/B prerequisite: $file"
    else
        [[ -f "$candidate" ]] || fail "missing A/B prerequisite: $file"
    fi
done
for path in etc etc/systemd etc/systemd/system etc/systemd/system/sockets.target.wants \
            etc/fstab usr/lib/flowerly \
            usr/lib/systemd/system usr/lib/systemd/system/flowerlyd.service.d \
            etc/systemd/system/multi-user.target.wants var/lib/flowerly efi; do
    [[ ! -L "$ROOTFS/$path" ]] || fail "symlink in Generation rootfs destination: $path"
done
for path in usr/lib/flowerly/generation-root.json usr/lib/flowerly/generation-ab-test-profile \
            usr/lib/flowerly/generation-ab-guest.py usr/lib/flowerly/generation-health-gate.sh \
            usr/lib/flowerly/generation-bless.sh \
            usr/lib/systemd/system/flowerly-generation-witness.service \
            usr/lib/systemd/system/flowerly-generation-health.service \
            usr/lib/flowerly/flowerly-slot-broker usr/lib/flowerly/slot-broker.json \
            usr/lib/systemd/system/flowerly-slot-broker.socket \
            usr/lib/systemd/system/flowerly-slot-broker@.service \
            etc/systemd/system/sockets.target.wants/flowerly-slot-broker.socket \
            usr/lib/systemd/system/flowerlyd.service.d/10-generation-witness.conf \
            etc/systemd/system/systemd-bless-boot.service \
            etc/systemd/system/multi-user.target.wants/flowerly-generation-witness.service \
            etc/systemd/system/multi-user.target.wants/flowerly-generation-health.service; do
    [[ ! -e "$ROOTFS/$path" && ! -L "$ROOTFS/$path" ]] ||
        fail "refusing existing A/B installation path: $path"
done
# The data partition MUST be a different mount from either root. Use the GPT
# partition label written by build-flowerly-ab-disk.sh; do not silently use
# rootfs state as fallback.
if [[ -f "$ROOTFS/etc/fstab" ]] &&
   grep -Eq '(^|[[:space:]])/var/lib/flowerly([[:space:]]|$)' "$ROOTFS/etc/fstab"; then
    fail "preexisting Flowerly data mount requires operator review"
fi
install -d -m 0755 \
    "$ROOTFS/usr/lib/flowerly" "$ROOTFS/usr/lib/systemd/system/flowerlyd.service.d" \
    "$ROOTFS/etc/systemd/system/sockets.target.wants" \
    "$ROOTFS/etc/systemd/system/multi-user.target.wants" "$ROOTFS/var/lib/flowerly" "$ROOTFS/efi"
install -m 0755 "$ROOT/distro/overlay/usr/lib/flowerly/generation-ab-guest.py" \
    "$ROOTFS/usr/lib/flowerly/generation-ab-guest.py"
install -m 0755 "$ROOT/distro/overlay/usr/lib/flowerly/generation-health-gate.sh" \
    "$ROOTFS/usr/lib/flowerly/generation-health-gate.sh"
install -m 0755 "$ROOT/distro/overlay/usr/lib/flowerly/generation-bless.sh" \
    "$ROOTFS/usr/lib/flowerly/generation-bless.sh"
for name in flowerly-generation-witness flowerly-generation-health; do
    install -m 0644 "$ROOT/distro/overlay/usr/lib/systemd/system/$name.service" \
        "$ROOTFS/usr/lib/systemd/system/$name.service"
    ln -s "/usr/lib/systemd/system/$name.service" \
        "$ROOTFS/etc/systemd/system/multi-user.target.wants/$name.service"
done
cat > "$ROOTFS/usr/lib/flowerly/generation-root.json" <<EOF
{"schema":"flowerly-generation-root/v2","slot":"$SLOT","root_partuuid":"$PARTUUID","source_commit":"$SOURCE","input_manifest_sha256":"$INPUT_SHA","build_id":"$BUILD_ID","schema_version":$SCHEMA_VERSION,"min_reader":$MIN_READER}
EOF
chmod 0644 "$ROOTFS/usr/lib/flowerly/generation-root.json"
# The root slot broker (ADR-0023 G4): test profile only until Gate D is
# verified. It reads the whole disk (virtio /dev/vda in the KVM tests) and the
# disk manifest the operator put on the ESP, and writes only boot entries.
install -m 0755 "$BROKER" "$ROOTFS/usr/lib/flowerly/flowerly-slot-broker"
install -m 0600 /dev/null "$ROOTFS/usr/lib/flowerly/slot-broker.json"
cat > "$ROOTFS/usr/lib/flowerly/slot-broker.json" <<'EOF'
{"disk":"/dev/vda","manifest":"/efi/flowerly/ab-disk.manifest.json","esp":"/efi","running_identity":"/usr/lib/flowerly/generation-root.json","test_profile_marker":"/usr/lib/flowerly/generation-ab-test-profile","passwd":"/etc/passwd","group":"/etc/group","cmdline":"/proc/cmdline","service_user":"flowerly","admin_group":"flowerly-admin"}
EOF
for name in flowerly-slot-broker.socket flowerly-slot-broker@.service; do
    install -m 0644 "$ROOT/distro/overlay/usr/lib/systemd/system/$name" \
        "$ROOTFS/usr/lib/systemd/system/$name"
done
ln -s /usr/lib/systemd/system/flowerly-slot-broker.socket \
    "$ROOTFS/etc/systemd/system/sockets.target.wants/flowerly-slot-broker.socket"
touch "$ROOTFS/usr/lib/flowerly/generation-ab-test-profile"
chmod 0644 "$ROOTFS/usr/lib/flowerly/generation-ab-test-profile"
cat > "$ROOTFS/usr/lib/systemd/system/flowerlyd.service.d/10-generation-witness.conf" <<'EOF'
[Unit]
Requires=flowerly-generation-witness.service
After=flowerly-generation-witness.service
EOF
chmod 0644 "$ROOTFS/usr/lib/systemd/system/flowerlyd.service.d/10-generation-witness.conf"
# The stock systemd-bless-boot.service can otherwise bless before Flowerly's
# health gate. Mask its unit but call the actual binary ONLY after IPC passes.
ln -s /dev/null "$ROOTFS/etc/systemd/system/systemd-bless-boot.service"
# The slot boots with a read-only root, so systemd-sysusers cannot create the
# service accounts at first boot (found by the third real KVM run: "user
# flowerly does not exist"). Bake them into the image instead.
SYSUSERS="${FLOWERLY_SYSUSERS:-systemd-sysusers}"
[[ -f "$ROOTFS/usr/lib/sysusers.d/flowerly.conf" && ! -L "$ROOTFS/usr/lib/sysusers.d/flowerly.conf" ]] ||
    fail "missing Flowerly sysusers.d configuration"
command -v "$SYSUSERS" >/dev/null 2>&1 || fail "missing host tool: systemd-sysusers"
"$SYSUSERS" --root="$ROOTFS" >/dev/null || fail "baking Flowerly service accounts failed"
# systemd-bless-boot renames the boot-counting entry on the ESP; without a
# mounted $BOOT it fails ("Couldn't find $BOOT partition", found by the first
# real KVM run). /boot is not usable: the root volume already holds files there.
printf '%s\n' \
    'PARTLABEL=data /var/lib/flowerly ext4 defaults,nosuid,nodev,x-systemd.device-timeout=20s 0 2' \
    'PARTLABEL=esp /efi vfat umask=0077,nosuid,nodev,noexec,x-systemd.device-timeout=20s 0 2' \
    >> "$ROOTFS/etc/fstab"
echo "Prepared test-only Generation A/B rootfs ($SLOT, $PARTUUID)."
echo "No firmware boot, slot activation, boot counting or fallback has been verified yet."
