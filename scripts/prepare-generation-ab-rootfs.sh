#!/usr/bin/env bash
# Explicit opt-in: prepare an already staged Arch rootfs as an A/B KVM test
# image. Not used by shipped distro or the existing direct-kernel boot probe.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
fail() { echo "Generation A/B rootfs: $*" >&2; exit 1; }
(( $# == 12 )) || fail "usage: ROOTFS --slot root-a|root-b --partuuid UUID --source-commit SHA40 --input-manifest-sha256 SHA64 --build-id ID --test-profile"
ROOTFS="$1"; shift
[[ "$1" == --slot ]] || fail "missing --slot"; SLOT="$2"; shift 2
[[ "$1" == --partuuid ]] || fail "missing --partuuid"; PARTUUID="$2"; shift 2
[[ "$1" == --source-commit ]] || fail "missing --source-commit"; SOURCE="$2"; shift 2
[[ "$1" == --input-manifest-sha256 ]] || fail "missing --input-manifest-sha256"; INPUT_SHA="$2"; shift 2
[[ "$1" == --build-id ]] || fail "missing --build-id"; BUILD_ID="$2"; shift 2
[[ "$1" == --test-profile ]] || fail "Generation A/B boot profile is test-only"
[[ "$SLOT" == root-a || "$SLOT" == root-b ]] || fail "invalid target slot"
[[ "$PARTUUID" =~ ^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$ ]] || fail "invalid root PARTUUID"
[[ "$SOURCE" =~ ^[0-9a-f]{40}$ && "$INPUT_SHA" =~ ^[0-9a-f]{64}$ ]] || fail "invalid source/input digest"
[[ "$BUILD_ID" =~ ^[A-Za-z0-9_.-]{1,64}$ ]] || fail "invalid build ID"
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
for path in etc etc/systemd etc/systemd/system etc/fstab usr/lib/flowerly \
            usr/lib/systemd/system usr/lib/systemd/system/flowerlyd.service.d \
            etc/systemd/system/multi-user.target.wants var/lib/flowerly efi; do
    [[ ! -L "$ROOTFS/$path" ]] || fail "symlink in Generation rootfs destination: $path"
done
for path in usr/lib/flowerly/generation-root.json usr/lib/flowerly/generation-ab-test-profile \
            usr/lib/flowerly/generation-ab-guest.py usr/lib/flowerly/generation-health-gate.sh \
            usr/lib/systemd/system/flowerly-generation-witness.service \
            usr/lib/systemd/system/flowerly-generation-health.service \
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
    "$ROOTFS/etc/systemd/system/multi-user.target.wants" "$ROOTFS/var/lib/flowerly" "$ROOTFS/efi"
install -m 0755 "$ROOT/distro/overlay/usr/lib/flowerly/generation-ab-guest.py" \
    "$ROOTFS/usr/lib/flowerly/generation-ab-guest.py"
install -m 0755 "$ROOT/distro/overlay/usr/lib/flowerly/generation-health-gate.sh" \
    "$ROOTFS/usr/lib/flowerly/generation-health-gate.sh"
for name in flowerly-generation-witness flowerly-generation-health; do
    install -m 0644 "$ROOT/distro/overlay/usr/lib/systemd/system/$name.service" \
        "$ROOTFS/usr/lib/systemd/system/$name.service"
    ln -s "/usr/lib/systemd/system/$name.service" \
        "$ROOTFS/etc/systemd/system/multi-user.target.wants/$name.service"
done
cat > "$ROOTFS/usr/lib/flowerly/generation-root.json" <<EOF
{"schema":"flowerly-generation-root/v1","slot":"$SLOT","root_partuuid":"$PARTUUID","source_commit":"$SOURCE","input_manifest_sha256":"$INPUT_SHA","build_id":"$BUILD_ID"}
EOF
chmod 0644 "$ROOTFS/usr/lib/flowerly/generation-root.json"
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
# systemd-bless-boot renames the boot-counting entry on the ESP; without a
# mounted $BOOT it fails ("Couldn't find $BOOT partition", found by the first
# real KVM run). /boot is not usable: the root volume already holds files there.
printf '%s\n' \
    'PARTLABEL=data /var/lib/flowerly ext4 defaults,nosuid,nodev,x-systemd.device-timeout=20s 0 2' \
    'PARTLABEL=esp /efi vfat umask=0077,nosuid,nodev,noexec,x-systemd.device-timeout=20s 0 2' \
    >> "$ROOTFS/etc/fstab"
echo "Prepared test-only Generation A/B rootfs ($SLOT, $PARTUUID)."
echo "No firmware boot, slot activation, boot counting or fallback has been verified yet."
