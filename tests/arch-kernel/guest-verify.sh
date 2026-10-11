#!/usr/bin/env bash
# Run inside the official Arch Linux VM after the guest has been fully upgraded
# and rebooted onto the installed Arch kernel.

set -euo pipefail

ROOT="${1:-$HOME/flowerly_os}"
cd "$ROOT"

REPORT="$ROOT/target/arch-kernel-report.txt"
mkdir -p "$(dirname "$REPORT")"
exec > >(tee "$REPORT") 2>&1

echo "== Flowerly Arch kernel system test =="
echo "timestamp_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "commit=${FLOWERLY_TEST_SHA:-$(cat .flowerly-tested-commit 2>/dev/null || echo exported-worktree)}"

# Prove that this really is an Arch userspace running an Arch kernel.
source /etc/os-release
if [[ "${ID:-}" != "arch" ]]; then
    echo "expected Arch Linux userspace, got ID=${ID:-unknown}" >&2
    exit 1
fi

kernel="$(uname -r)"
if [[ "$kernel" != *arch* ]]; then
    echo "expected an Arch kernel release string, got: $kernel" >&2
    exit 1
fi
if [[ ! -d "/usr/lib/modules/$kernel" ]]; then
    echo "running kernel has no matching /usr/lib/modules/$kernel tree" >&2
    exit 1
fi

echo "os_pretty_name=${PRETTY_NAME}"
echo "kernel=$kernel"
echo "kernel_package=$(pacman -Q linux)"
echo "systemd=$(systemctl --version | head -n1)"
echo "rustc=$(rustc --version)"
echo "cargo=$(cargo --version)"
echo "just=$(just --version)"

echo
echo "== Formatting =="
just fmt-check

echo
echo "== Clippy =="
just lint

echo
echo "== Compile check =="
just check

echo
echo "== Rust tests =="
just test

echo
echo "== Release build =="
just build

echo
echo "== Documentation inputs =="
bash ./scripts/check-docs.sh

echo
echo "== Arch image inputs =="
bash ./scripts/check-image-inputs.sh

echo
echo "== Full daemon runtime smoke test =="
cleanup_runtime() {
    ./scripts/dev-runtime.sh stop >/dev/null 2>&1 || true
}
trap cleanup_runtime EXIT

./scripts/dev-runtime.sh start

runtime_dir="$ROOT/target/flowerly-dev"
services=(
    agenticd
    flowd
    sessiond
    goald
    provenanced
    capabilityd
    generationd
    evolutiond
)

for service in "${services[@]}"; do
    echo "-- status: $service"
    "$ROOT/target/release/agenticctl" \
        --runtime-dir "$runtime_dir" \
        status "$service"
done

./scripts/dev-runtime.sh stop
trap - EXIT

echo
echo "ARCH_KERNEL_SYSTEM_TEST=PASS"
