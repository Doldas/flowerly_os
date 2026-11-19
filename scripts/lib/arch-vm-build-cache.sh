#!/usr/bin/env bash
# Sourced by arch-kernel-vm-test.sh; deliberately no side effects on import.
#
# A trusted release runner can attach a sparse ext4 image labelled
# FLOWERLY_BUILD for Cargo outputs, the registry and signed pacman downloads.
# Cargo checks its build fingerprints and pacman verifies selected packages
# against the freshly synchronized repositories. An unclean run can leave
# torn files: rotate the image and use CARGO_INCREMENTAL=0.

FLOWERLY_BUILD_CACHE_LABEL=FLOWERLY_BUILD
FLOWERLY_BUILD_CACHE_MAX_AGE_DAYS="${FLOWERLY_BUILD_CACHE_MAX_AGE_DAYS:-14}"
FLOWERLY_BUILD_CACHE_MAX_GB="${FLOWERLY_BUILD_CACHE_MAX_GB:-20}"
FLOWERLY_BUILD_CACHE_SIZE_GB="${FLOWERLY_BUILD_CACHE_SIZE_GB:-28}"

# The image's name carries a format version so that an image written under
# older rules (which cached incremental data) is never reused.
FLOWERLY_BUILD_CACHE_FILE=build-cache-v3.raw

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
    mkdir -p -m 0700 "$dir" || return 0
    local mode
    [[ -d "$dir" && ! -L "$dir" && -O "$dir" ]] || return 0
    mode="$(stat -c %a "$dir")" || return 0
    (( (8#$mode & 0022) == 0 )) || return 0
    local image="$dir/$FLOWERLY_BUILD_CACHE_FILE" state=warm
    # One VM at a time may own the image; a second run goes without a cache.
    exec {FLOWERLY_BUILD_CACHE_LOCK_FD}>"$dir/build-cache.lock" || return 0
    if ! flock -n "$FLOWERLY_BUILD_CACHE_LOCK_FD"; then
        FLOWERLY_BUILD_CACHE_STATE=locked
        return 0
    fi
    rm -f "$dir/build-cache.raw" "$dir/build-cache-v2.raw"   # obsolete layouts
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
    # Dirty until the run ends cleanly; a crash leaves it behind.
    : > "$dir/build-cache.dirty" || return 0
    FLOWERLY_BUILD_CACHE_IMAGE="$image"
    FLOWERLY_BUILD_CACHE_STATE="$state"
    FLOWERLY_BUILD_CACHE_DIR="$dir"
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

# The same labelled disk is mounted before the upgrade and again after reboot.
# pacman retains its ordinary repositories, signature checks and full upgrade.
flowerly_build_cache_guest_mount() {
    cat <<'GUEST'
set -euo pipefail
sudo mkdir -p /mnt/build-cache
sudo mount -o noatime,nodev,nosuid LABEL=FLOWERLY_BUILD /mnt/build-cache
GUEST
}

flowerly_build_cache_guest_attach() {
    flowerly_build_cache_guest_mount
    cat <<'GUEST'
sudo install -d -m 0755 -o "$(id -u)" -g "$(id -g)" \
    /mnt/build-cache/debug /mnt/build-cache/release /mnt/build-cache/cargo-home
mkdir -p "$HOME/flowerly_os/target"
ln -s /mnt/build-cache/debug "$HOME/flowerly_os/target/debug"
ln -s /mnt/build-cache/release "$HOME/flowerly_os/target/release"
GUEST
}
