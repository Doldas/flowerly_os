#!/usr/bin/env bash
# Offline regression for the Arch VM build cache (#285): no VM, no network.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT/scripts/lib/arch-vm-build-cache.sh"
fail() { echo "build cache test: $*" >&2; exit 1; }
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
export FLOWERLY_BUILD_CACHE_SIZE_GB=1   # sparse, so cheap
prep() { flowerly_build_cache_release; flowerly_build_cache_prepare "$@"; }
label() { PATH="$PATH:/usr/sbin:/sbin" dumpe2fs -h "$1" 2>/dev/null | sed -n 's/^Filesystem volume name:[[:space:]]*//p'; }

prep "$tmp/c" 0
[[ "$FLOWERLY_BUILD_CACHE_STATE" == disabled && -z "$FLOWERLY_BUILD_CACHE_IMAGE" ]] || fail "must be opt-in"
[[ ! -e "$tmp/c" ]] || fail "disabled must not touch the disk"
prep relative/path 1
[[ "$FLOWERLY_BUILD_CACHE_STATE" == unavailable ]] || fail "relative directory accepted"
prep '' 1
[[ "$FLOWERLY_BUILD_CACHE_STATE" == unavailable ]] || fail "empty directory accepted"
mkdir "$tmp/shared"
chmod 0777 "$tmp/shared"
prep "$tmp/shared" 1
[[ "$FLOWERLY_BUILD_CACHE_STATE" == unavailable ]] || fail "shared directory accepted"
ln -s "$tmp/shared" "$tmp/symlink"
chmod 0700 "$tmp/shared"
prep "$tmp/symlink" 1
[[ "$FLOWERLY_BUILD_CACHE_STATE" == unavailable ]] || fail "symlinked directory accepted"

prep "$tmp/c" 1
[[ "$FLOWERLY_BUILD_CACHE_STATE" == cold ]] || fail "first use must be cold, got $FLOWERLY_BUILD_CACHE_STATE"
image="$tmp/c/build-cache-v3.raw"
[[ "$FLOWERLY_BUILD_CACHE_IMAGE" == "$image" && -f "$image" && "$(label "$image")" == FLOWERLY_BUILD ]] ||
    fail "image is not a labelled ext4"
[[ "$(stat -c %s "$image")" == 1073741824 ]] || fail "unexpected image size"

printf marker | dd of="$image" bs=1 seek=4194304 conv=notrunc status=none
# A run that never finished (crash) leaves the cache dirty: the next one is cold.
prep "$tmp/c" 1
[[ "$FLOWERLY_BUILD_CACHE_STATE" == rotated-unclean ]] || fail "unclean cache reused, got $FLOWERLY_BUILD_CACHE_STATE"
[[ "$(dd if="$image" bs=1 skip=4194304 count=6 status=none | tr -d "\0")" != marker ]] || fail "dirty cache content survived"
printf marker | dd of="$image" bs=1 seek=4194304 conv=notrunc status=none
flowerly_build_cache_mark_clean
prep "$tmp/c" 1
[[ "$FLOWERLY_BUILD_CACHE_STATE" == warm ]] || fail "second use must be warm, got $FLOWERLY_BUILD_CACHE_STATE"
[[ "$(dd if="$image" bs=1 skip=4194304 count=6 status=none)" == marker ]] || fail "warm cache lost its content"

touch -d '30 days ago' "$image"
flowerly_build_cache_mark_clean
prep "$tmp/c" 1
[[ "$FLOWERLY_BUILD_CACHE_STATE" == rotated-age ]] || fail "old cache must rotate"
[[ "$(label "$image")" == FLOWERLY_BUILD ]] || fail "rotated image is not usable"

flowerly_build_cache_mark_clean
FLOWERLY_BUILD_CACHE_MAX_GB=0 prep "$tmp/c" 1
[[ "$FLOWERLY_BUILD_CACHE_STATE" == rotated-size ]] || fail "oversized cache must rotate"

# An image written under the old name is deleted, never reused.
flowerly_build_cache_mark_clean
printf old > "$tmp/c/build-cache.raw"
printf old > "$tmp/c/build-cache-v2.raw"
prep "$tmp/c" 1
[[ ! -e "$tmp/c/build-cache.raw" && ! -e "$tmp/c/build-cache-v2.raw" ]] || fail "obsolete image kept"

# A symlinked image is never followed; it is replaced.
flowerly_build_cache_mark_clean
rm -f "$image"; ln -s /etc/hostname "$image"
prep "$tmp/c" 1
[[ "$FLOWERLY_BUILD_CACHE_STATE" == cold && ! -L "$image" ]] || fail "symlinked image kept"

# A second run while the first holds the lock goes without a cache.
flowerly_build_cache_release
exec 9>"$tmp/c/build-cache.lock"; flock -n 9
FLOWERLY_BUILD_CACHE_LOCK_FD="" flowerly_build_cache_prepare "$tmp/c" 1
[[ "$FLOWERLY_BUILD_CACHE_STATE" == locked && -z "$FLOWERLY_BUILD_CACHE_IMAGE" ]] || fail "lock not honoured"
exec 9>&-

# The guest retains cacheable build profiles, keeping runtime state and reports
# in the disposable checkout. Both mounts use the same restricted options.
mount_script="$(flowerly_build_cache_guest_mount)"
attach_script="$(flowerly_build_cache_guest_attach)"
grep -qFx 'sudo mount -o noatime,nodev,nosuid LABEL=FLOWERLY_BUILD /mnt/build-cache' <<<"$mount_script" ||
    fail "guest mount lacks the required label or options"
[[ "$attach_script" == "$mount_script"$'\n'* ]] || fail "mount after reboot differs"
for profile in debug release; do
    grep -qFx "ln -s /mnt/build-cache/$profile \"\$HOME/flowerly_os/target/$profile\"" <<<"$attach_script" ||
        fail "guest attach lacks $profile output link"
done
bash -n <<<"$attach_script" || fail "guest attach is not valid shell"
echo "Arch VM build cache: opt-in, labelled ext4, warm/cold/rotated/locked states, guest attach layout"
