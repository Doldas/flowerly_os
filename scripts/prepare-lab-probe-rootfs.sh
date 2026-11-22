#!/usr/bin/env bash
# Explicit opt-in: add the Evolution Lab guest probe (ADR-0024 D2) to a rootfs
# that prepare-generation-ab-rootfs.sh already made an A/B test image. Every
# boot of such a slot reports on the console and powers off, so only a lab
# candidate build installs it, in both slots. Never the shipped distro.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
fail() { echo "Evolution Lab probe rootfs: $*" >&2; exit 1; }
(( $# == 2 )) || fail "usage: ROOTFS --test-profile"
ROOTFS="$1"
[[ "$2" == --test-profile ]] || fail "the Evolution Lab probe is test-only"
[[ -d "$ROOTFS" && ! -L "$ROOTFS" ]] || fail "missing/symlinked rootfs"
# The A/B test profile provides the health gate, the Admin drop-in the probe's
# account relies on, and the baked identity it hashes.
for file in usr/lib/flowerly/generation-ab-test-profile usr/lib/flowerly/generation-root.json \
            usr/lib/systemd/system/flowerly-generation-health.service \
            etc/systemd/system/flowerlyd.service.d/50-multi-user.conf \
            usr/lib/sysusers.d/flowerly.conf usr/bin/python3 usr/bin/runuser; do
    [[ -e "$ROOTFS/$file" || -L "$ROOTFS/$file" ]] ||
        fail "not a prepared A/B test rootfs (missing $file)"
done
for path in usr usr/lib usr/lib/flowerly usr/lib/systemd usr/lib/systemd/system \
            usr/lib/sysusers.d etc etc/systemd etc/systemd/system \
            etc/systemd/system/multi-user.target.wants; do
    [[ -d "$ROOTFS/$path" && ! -L "$ROOTFS/$path" ]] ||
        fail "missing or symlinked rootfs destination: $path"
done
for path in usr/lib/flowerly/lab-probe.py usr/lib/systemd/system/flowerly-lab-probe.service \
            etc/systemd/system/multi-user.target.wants/flowerly-lab-probe.service \
            usr/lib/sysusers.d/flowerly-lab-probe.conf; do
    [[ ! -e "$ROOTFS/$path" && ! -L "$ROOTFS/$path" ]] ||
        fail "refusing existing probe installation path: $path"
done
SYSUSERS="${FLOWERLY_SYSUSERS:-systemd-sysusers}"
command -v "$SYSUSERS" >/dev/null 2>&1 || fail "missing host tool: systemd-sysusers"
install -m 0755 "$ROOT/testbed/evolution-world/lab-probe.py" "$ROOTFS/usr/lib/flowerly/lab-probe.py"
install -m 0644 "$ROOT/testbed/evolution-world/systemd/flowerly-lab-probe.service" \
    "$ROOTFS/usr/lib/systemd/system/flowerly-lab-probe.service"
ln -s /usr/lib/systemd/system/flowerly-lab-probe.service \
    "$ROOTFS/etc/systemd/system/multi-user.target.wants/flowerly-lab-probe.service"
install -m 0644 "$ROOT/testbed/evolution-world/sysusers/flowerly-lab-probe.conf" \
    "$ROOTFS/usr/lib/sysusers.d/flowerly-lab-probe.conf"
# The slot boots read-only, so the probe's account is baked in now, as the
# A/B preparation bakes the others.
"$SYSUSERS" --root="$ROOTFS" >/dev/null || fail "baking the probe account failed"
echo "Added the test-only Evolution Lab probe to $ROOTFS."
echo "Every boot of this rootfs now reports on the console and powers off."
