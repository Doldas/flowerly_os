#!/usr/bin/env bash
# Sourced by arch-kernel-vm-test.sh; deliberately no side effects on import.
#
# A persistent build cache for the Arch guest (#285): a sparse raw ext4 image,
# labelled FLOWERLY_BUILD, that the guest mounts for its cargo target
# directory and registry. Without it every run compiles all dependencies in a
# fresh VM (about 26 of the 53 minutes). cargo's own fingerprints decide what is
# reused, so a cache hit cannot make a stale build pass; the cache only ever
# saves work. It is opt-in, never fails the run, and is thrown away when it is
# old or large (a rolling Arch moves rustc and system libraries under it), and
# whenever the previous run did not end cleanly: a guest or QEMU that died
# mid-build leaves torn files, and rustc's incremental cache then fails with an
# internal compiler error (seen on 51f6e6b after a QEMU segfault). Incremental
# compilation is also switched off in the guest (CARGO_INCREMENTAL=0).

FLOWERLY_BUILD_CACHE_LABEL=FLOWERLY_BUILD
FLOWERLY_BUILD_CACHE_MAX_AGE_DAYS="${FLOWERLY_BUILD_CACHE_MAX_AGE_DAYS:-14}"
FLOWERLY_BUILD_CACHE_MAX_GB="${FLOWERLY_BUILD_CACHE_MAX_GB:-20}"
FLOWERLY_BUILD_CACHE_SIZE_GB="${FLOWERLY_BUILD_CACHE_SIZE_GB:-28}"

# The image's name carries a format version so that an image written under
# older rules (which cached incremental data) is never reused.
FLOWERLY_BUILD_CACHE_FILE=build-cache-v2.raw

# Call it directly (not in a command substitution: the lock must belong to the
# caller). Sets FLOWERLY_BUILD_CACHE_STATE to one of disabled, cold, warm,
# rotated-age, rotated-size, rotated-unclean, locked, unavailable, and FLOWERLY_BUILD_CACHE_IMAGE
# to the image path (empty unless the cache is usable).
flowerly_build_cache_prepare() {
    local dir="${1:-}" enabled="${2:-0}"
    FLOWERLY_BUILD_CACHE_IMAGE=""
    FLOWERLY_BUILD_CACHE_STATE=unavailable
    FLOWERLY_BUILD_CACHE_DIR=""
    if [[ "$enabled" != 1 ]]; then
        FLOWERLY_BUILD_CACHE_STATE=disabled
        return 0
    fi
    local tool
    for tool in mkfs.ext4 truncate flock stat; do
        if ! PATH="$PATH:/usr/sbin:/sbin" command -v "$tool" >/dev/null 2>&1; then
            FLOWERLY_BUILD_CACHE_STATE=unavailable
            return 0
        fi
    done
    if [[ -z "$dir" || "$dir" != /* ]]; then
        FLOWERLY_BUILD_CACHE_STATE=unavailable
        return 0
    fi
    mkdir -p "$dir" || return 0
    local image="$dir/$FLOWERLY_BUILD_CACHE_FILE" state=warm
    # One VM at a time may own the image; a second run goes without a cache.
    exec {FLOWERLY_BUILD_CACHE_LOCK_FD}>"$dir/build-cache.lock" || return 0
    if ! flock -n "$FLOWERLY_BUILD_CACHE_LOCK_FD"; then
        FLOWERLY_BUILD_CACHE_STATE=locked
        return 0
    fi
    rm -f "$dir/build-cache.raw"   # pre-v2 image: may hold torn incremental data
    if [[ -e "$image" && ! -L "$image" ]]; then
        local age_days allocated_gb
        age_days=$(( ( $(date +%s) - $(stat -c %Y "$image") ) / 86400 ))
        allocated_gb=$(( $(stat -c %b "$image") * 512 / 1073741824 ))
        if [[ -e "$dir/build-cache.dirty" ]]; then
            rm -f "$image"; state=rotated-unclean
        elif (( age_days >= FLOWERLY_BUILD_CACHE_MAX_AGE_DAYS )); then
            rm -f "$image"; state=rotated-age
        elif (( allocated_gb >= FLOWERLY_BUILD_CACHE_MAX_GB )); then
            rm -f "$image"; state=rotated-size
        fi
    elif [[ -e "$image" || -L "$image" ]]; then
        rm -f "$image"
        state=cold
    else
        state=cold
    fi
    if [[ ! -e "$image" ]]; then
        if ! { truncate -s "${FLOWERLY_BUILD_CACHE_SIZE_GB}G" "$image" &&
               PATH="$PATH:/usr/sbin:/sbin" mkfs.ext4 -q -F -L "$FLOWERLY_BUILD_CACHE_LABEL" "$image"; }; then
            rm -f "$image"
            FLOWERLY_BUILD_CACHE_STATE=unavailable
            return 0
        fi
        [[ "$state" == warm ]] && state=cold
    fi
    FLOWERLY_BUILD_CACHE_IMAGE="$image"
    FLOWERLY_BUILD_CACHE_STATE="$state"
    FLOWERLY_BUILD_CACHE_DIR="$dir"
    # Dirty until the run ends cleanly; a crash leaves it behind.
    : > "$dir/build-cache.dirty"
}

# Called only when the whole run succeeded and the VM powered off by itself.
flowerly_build_cache_mark_clean() {
    [[ -n "${FLOWERLY_BUILD_CACHE_DIR:-}" ]] && rm -f "$FLOWERLY_BUILD_CACHE_DIR/build-cache.dirty"
    return 0
}

# Gives the image back (closing the lock descriptor); process exit does too.
flowerly_build_cache_release() {
    if [[ -n "${FLOWERLY_BUILD_CACHE_LOCK_FD:-}" ]]; then
        eval "exec ${FLOWERLY_BUILD_CACHE_LOCK_FD}>&-"
        FLOWERLY_BUILD_CACHE_LOCK_FD=""
    fi
}

# The command the guest runs to make the cache its target directory. Cargo
# then finds target/ in the checkout as usual (relative paths in the tests keep
# working) and keeps the registry beside it.
flowerly_build_cache_guest_attach() {
    cat <<'GUEST'
set -euo pipefail
sudo mkdir -p /mnt/build-cache
sudo mount -o noatime LABEL=FLOWERLY_BUILD /mnt/build-cache
sudo chown "$(id -u):$(id -g)" /mnt/build-cache
mkdir -p /mnt/build-cache/target /mnt/build-cache/cargo-home
chmod 755 /mnt/build-cache /mnt/build-cache/target
rm -rf "$HOME/flowerly_os/target"
ln -s /mnt/build-cache/target "$HOME/flowerly_os/target"
GUEST
}
