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
echo "python=$(python3 --version)"

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
echo "== #225 cross-UID authorization and per-user ownership with real Arch UIDs =="
sudo python3 scripts/test-admin-authorization.py --cross-uid target/release
sudo python3 scripts/test-user-ownership.py --cross-uid target/release

echo
echo "== #225 real systemd admission: shipped default, opt-in drop-in, disable =="
# The guest is disposable: this installs flowerlyd, its unit and test users.
sudo env FLOWERLY_DISPOSABLE_HOST=1 \
    FLOWERLY_TEST_SHA="${FLOWERLY_TEST_SHA:-$(cat .flowerly-tested-commit 2>/dev/null || echo unknown)}" \
    python3 scripts/test-systemd-multi-user.py target/release

echo
echo "== PTY root-refusal negative, unprivileged ownership and adversarial stream/crash tests =="
sudo python3 scripts/test-pty-guardian.py --expect-root-denied
python3 scripts/test-pty-guardian.py
python3 scripts/test-pty-adversarial.py target/release
python3 scripts/test-sessiond-pty-flow.py target/release

echo
echo "== Production delegated cgroup v2, same-UID escape, setsid descendant and guardian crash =="
acceptance_user=flowerly-pty-ci
shell_user=flowerly-pty-shell-ci
acceptance_root=/opt/flowerly-pty-acceptance
if ! id "$acceptance_user" >/dev/null 2>&1; then
    sudo useradd --system --user-group --create-home \
        --home-dir /var/lib/flowerly-pty-ci \
        --shell "$(command -v nologin)" "$acceptance_user"
fi
if ! id "$shell_user" >/dev/null 2>&1; then
    sudo useradd --system --user-group --create-home \
        --home-dir /var/lib/flowerly-pty-shell-ci \
        --shell "$(command -v nologin)" "$shell_user"
fi
if [[ "$(id -Gn "$shell_user")" != "$shell_user" ]]; then
    echo "PTY shell acceptance user must have no supplementary groups" >&2
    exit 1
fi
if [[ "$(id -u "$shell_user")" == "$(id -u "$acceptance_user")" ]]; then
    echo "PTY shell must not share the trusted manager UID" >&2
    exit 1
fi
if [[ "$(id -Gn "$acceptance_user")" != "$acceptance_user" ]]; then
    echo "PTY acceptance user must have no supplementary groups" >&2
    exit 1
fi
sudo install -d -m 0755 "$acceptance_root/scripts"
sudo install -m 0644 scripts/arch-pty-cgroup-acceptance.sh \
    scripts/test-pty-cgroup.py scripts/pty-guardian.py \
    "$acceptance_root/scripts"
(
    cd "$acceptance_root"
    sudo bash scripts/arch-pty-cgroup-acceptance.sh --system "$acceptance_user" "$shell_user"
)

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

# Pin the runtime directory instead of inheriting the session default:
# dev-runtime.sh otherwise resolves it from XDG_RUNTIME_DIR, so an SSH
# session with XDG_RUNTIME_DIR set would start the daemons at
# /run/user/<uid>/flowerly-dev while the explicit agenticctl calls below
# would look for them under the repository target directory.
runtime_dir="$ROOT/target/flowerly-dev"
export RUNTIME_DIR="${RUNTIME_DIR:-$runtime_dir}"
runtime_dir="$RUNTIME_DIR"

./scripts/dev-runtime.sh start
services=(
    flowerlyd
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
