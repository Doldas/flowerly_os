#!/usr/bin/env bash
# Runs INSIDE the Arch guest, after cloud-init and BEFORE pacman -Syu.
# Protects the release CI from using a tiny cloud-image root partition.
set -euo pipefail

min_total_gb="${1:-16}"
min_free_gb="${2:-8}"
if [[ ! "$min_total_gb" =~ ^[1-9][0-9]?$ || ! "$min_free_gb" =~ ^[1-9][0-9]?$ ]]; then
    echo "disk preflight thresholds must be positive whole GiB" >&2
    exit 2
fi

echo "== Guest root filesystem geometry before package upgrade =="
df -hT /
lsblk -o NAME,SIZE,TYPE,FSTYPE,MOUNTPOINTS || true
printf 'growpart_available='
if command -v growpart >/dev/null 2>&1; then
    echo yes
else
    echo no
fi

read -r total_bytes available_bytes < <(
    df -B1 --output=size,avail / | awk 'NR == 2 {print $1, $2}'
)
if [[ ! "$total_bytes" =~ ^[0-9]+$ || ! "$available_bytes" =~ ^[0-9]+$ ]]; then
    echo "could not determine root filesystem size and free bytes" >&2
    exit 1
fi

required_total=$((min_total_gb * 1024 * 1024 * 1024))
required_free=$((min_free_gb * 1024 * 1024 * 1024))
echo "root_size_bytes=$total_bytes minimum_bytes=$required_total"
echo "root_available_bytes=$available_bytes minimum_bytes=$required_free"
if (( total_bytes < required_total || available_bytes < required_free )); then
    echo "Arch guest root is still too small for pacman and Rust build." >&2
    echo "qemu-img resize only grows the virtual disk; cloud-init must growpart / and resize_rootfs." >&2
    echo "On Arch, growpart requires the cloud-guest-utils optional package." >&2
    echo "Inspect cloud-init logs, partition geometry, and the guest-disk-before-upgrade report." >&2
    exit 1
fi

echo "Arch guest disk preflight OK"
