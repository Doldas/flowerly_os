#!/usr/bin/env bash
# No VM, network, or host disk allocation needed for this regression check.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT/scripts/lib/arch-vm-disk.sh"
for value in 24 32 64 256; do
    flowerly_arch_vm_validate_disk_gb "$value"
done
for value in 0 1 16 23 257 1024 01 -1 foo 32G '32.0' ''; do
    if flowerly_arch_vm_validate_disk_gb "$value" >/dev/null 2>&1; then
        echo "accepted invalid Arch VM capacity: '$value'" >&2
        exit 1
    fi
done
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/mock-bin"
cat >"$tmp/mock-bin/qemu-img" <<'MOCK'
#!/usr/bin/env bash
printf '%s\n' "$@" >"$FLOWERLY_QEMU_IMG_MOCK_ARGS"
MOCK
chmod +x "$tmp/mock-bin/qemu-img"
export PATH="$tmp/mock-bin:$PATH"
export FLOWERLY_QEMU_IMG_MOCK_ARGS="$tmp/qemu-args"
flowerly_arch_vm_resize_overlay "$tmp/overlay.qcow2" 32
mapfile -t got <"$FLOWERLY_QEMU_IMG_MOCK_ARGS"
[[ "${got[*]}" == "resize -f qcow2 $tmp/overlay.qcow2 32G" ]] || {
    echo "unexpected qemu-img resize invocation: ${got[*]}" >&2
    exit 1
}

# Exercise the actual guest preflight against synthetic df output.
cat >"$tmp/mock-bin/df" <<'MOCK'
#!/usr/bin/env bash
if [[ "$1" == "-B1" ]]; then
    printf '1B-blocks Avail\n%s %s\n' "$MOCK_ROOT_TOTAL" "$MOCK_ROOT_FREE"
else
    printf 'Filesystem Type Size Used Avail Use%% Mounted on\nmock ext4 32G 1G 31G 4%% /\n'
fi
MOCK
chmod +x "$tmp/mock-bin/df"
export MOCK_ROOT_TOTAL=$((32 * 1024 * 1024 * 1024))
export MOCK_ROOT_FREE=$((28 * 1024 * 1024 * 1024))
bash "$ROOT/scripts/arch-vm-guest-disk-preflight.sh" 16 8 >"$tmp/healthy.log"

export MOCK_ROOT_TOTAL=$((2 * 1024 * 1024 * 1024))
export MOCK_ROOT_FREE=$((1 * 1024 * 1024 * 1024))
if bash "$ROOT/scripts/arch-vm-guest-disk-preflight.sh" 16 8 >"$tmp/tiny.log" 2>&1; then
    echo "tiny root filesystem incorrectly passed guest preflight" >&2
    exit 1
fi
export MOCK_ROOT_TOTAL=$((32 * 1024 * 1024 * 1024))
if bash "$ROOT/scripts/arch-vm-guest-disk-preflight.sh" 16 8 >"$tmp/full.log" 2>&1; then
    echo "nearly full root filesystem incorrectly passed guest preflight" >&2
    exit 1
fi

echo "Arch VM disk capacity, sparse overlay resize and guest preflight contracts OK"
