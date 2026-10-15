#!/usr/bin/env bash
# Boot the official Arch Linux cloud image under QEMU/KVM, fully upgrade it,
# reboot into the installed Arch kernel, then run the complete Flowerly
# repository verification inside that guest.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEST_REF="${FLOWERLY_TEST_REF:-unknown}"
TEST_SHA="${FLOWERLY_TEST_SHA:-$(git -C "$ROOT" rev-parse HEAD)}"
TEST_SLUG="${FLOWERLY_TEST_SLUG:-manual}"

CHECKOUT_SHA="$(git -C "$ROOT" rev-parse HEAD)"
if [[ "$TEST_SHA" != "$CHECKOUT_SHA" ]]; then
    echo "requested Arch verification commit $TEST_SHA does not match checked-out commit $CHECKOUT_SHA" >&2
    exit 1
fi

ARCH_IMAGE_URL="${ARCH_IMAGE_URL:-https://geo.mirror.pkgbuild.com/images/latest/Arch-Linux-x86_64-cloudimg.qcow2}"
ARCH_IMAGE_SHA_URL="${ARCH_IMAGE_SHA_URL:-$ARCH_IMAGE_URL.SHA256}"
CACHE_DIR="${ARCH_VM_CACHE_DIR:-$HOME/.cache/flowerly/arch-images}"
WORK_DIR="${ARCH_VM_WORK_DIR:-$ROOT/target/arch-kernel/$TEST_SLUG}"
REPORT_DIR="$WORK_DIR/report"
SSH_PORT="${ARCH_VM_SSH_PORT:-22222}"
VM_CPUS="${ARCH_VM_CPUS:-4}"
VM_MEMORY_MB="${ARCH_VM_MEMORY_MB:-6144}"

mkdir -p "$CACHE_DIR"
rm -rf "$REPORT_DIR"
mkdir -p "$REPORT_DIR"
rm -f "$WORK_DIR/qemu.pid" "$WORK_DIR/overlay.qcow2" "$WORK_DIR/seed.img"
rm -rf "$WORK_DIR/repo-export"
mkdir -p "$WORK_DIR/repo-export"

for tool in curl git gzip qemu-img qemu-system-x86_64 cloud-localds ssh scp ssh-keygen tar; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "required host tool is missing: $tool" >&2
        exit 1
    fi
done

checksum_text="$(curl --fail --location --silent --show-error --retry 4 "$ARCH_IMAGE_SHA_URL")"
expected_sha="$(awk 'NF {print $1; exit}' <<<"$checksum_text")"
if [[ ! "$expected_sha" =~ ^[0-9a-fA-F]{64}$ ]]; then
    echo "could not parse Arch cloud-image SHA256 from $ARCH_IMAGE_SHA_URL" >&2
    exit 1
fi
expected_sha="${expected_sha,,}"

base_image="$CACHE_DIR/Arch-Linux-x86_64-cloudimg-$expected_sha.qcow2"
if [[ -f "$base_image" ]]; then
    actual_sha="$(sha256sum "$base_image" | awk '{print $1}')"
    if [[ "$actual_sha" != "$expected_sha" ]]; then
        echo "cached Arch image checksum mismatch; refreshing" >&2
        rm -f "$base_image"
    fi
fi

if [[ ! -f "$base_image" ]]; then
    tmp_image="$base_image.part"
    rm -f "$tmp_image"
    echo "downloading official Arch Linux cloud image"
    curl --fail --location --show-error --retry 4 \
        --output "$tmp_image" "$ARCH_IMAGE_URL"
    echo "$expected_sha  $tmp_image" | sha256sum --check --status
    mv "$tmp_image" "$base_image"
fi

overlay="$WORK_DIR/overlay.qcow2"
qemu-img create -q -f qcow2 -F qcow2 -b "$base_image" "$overlay"

ssh_key="$WORK_DIR/id_ed25519"
rm -f "$ssh_key" "$ssh_key.pub"
ssh-keygen -q -t ed25519 -N "" -f "$ssh_key" -C "flowerly-arch-kernel-ci"
public_key="$(cat "$ssh_key.pub")"

cat >"$WORK_DIR/user-data" <<EOF
#cloud-config
users:
  - name: flowerly
    gecos: Flowerly CI
    groups: [wheel]
    shell: /bin/bash
    sudo: ALL=(ALL) NOPASSWD:ALL
    ssh_authorized_keys:
      - $public_key
ssh_pwauth: false
disable_root: true
EOF

cat >"$WORK_DIR/meta-data" <<EOF
instance-id: flowerly-arch-kernel-$TEST_SLUG
local-hostname: flowerly-arch
EOF

cloud-localds "$WORK_DIR/seed.img" "$WORK_DIR/user-data" "$WORK_DIR/meta-data"

find_first() {
    local candidate
    for candidate in "$@"; do
        if [[ -f "$candidate" ]]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

ovmf_code="$(find_first \
    /usr/share/OVMF/OVMF_CODE_4M.fd \
    /usr/share/OVMF/OVMF_CODE.fd \
    /usr/share/edk2/ovmf/OVMF_CODE.fd \
    /usr/share/edk2/x64/OVMF_CODE.4m.fd)" || {
        echo "OVMF firmware not found; install the host ovmf package" >&2
        exit 1
    }

ovmf_vars_template="$(find_first \
    /usr/share/OVMF/OVMF_VARS_4M.fd \
    /usr/share/OVMF/OVMF_VARS.fd \
    /usr/share/edk2/ovmf/OVMF_VARS.fd \
    /usr/share/edk2/x64/OVMF_VARS.4m.fd)" || {
        echo "OVMF variable store not found; install the host ovmf package" >&2
        exit 1
    }

ovmf_vars="$WORK_DIR/OVMF_VARS.fd"
cp "$ovmf_vars_template" "$ovmf_vars"

pidfile="$WORK_DIR/qemu.pid"
qemu_log="$REPORT_DIR/qemu-serial.log"
: >"$qemu_log"

accel="tcg"
accel_args=(-accel "tcg,thread=multi" -cpu max)
if [[ -c /dev/kvm && -r /dev/kvm && -w /dev/kvm ]]; then
    accel="kvm"
    accel_args=(-accel kvm -cpu host)
fi

launch_vm() {
    local selected_accel="$1"
    shift
    echo "starting Arch VM with accelerator=$selected_accel"
    qemu-system-x86_64 \
        -name "flowerly-arch-$TEST_SLUG" \
        -machine q35 \
        "$@" \
        -smp "$VM_CPUS" \
        -m "$VM_MEMORY_MB" \
        -drive "if=pflash,format=raw,readonly=on,file=$ovmf_code" \
        -drive "if=pflash,format=raw,file=$ovmf_vars" \
        -drive "if=virtio,format=qcow2,file=$overlay" \
        -drive "if=virtio,format=raw,readonly=on,file=$WORK_DIR/seed.img" \
        -netdev "user,id=net0,hostfwd=tcp:127.0.0.1:$SSH_PORT-:22" \
        -device virtio-net-pci,netdev=net0 \
        -display none \
        -serial "file:$qemu_log" \
        -monitor none \
        -daemonize \
        -pidfile "$pidfile"
}

if ! launch_vm "$accel" "${accel_args[@]}"; then
    if [[ "$accel" == "kvm" ]]; then
        echo "KVM launch failed; retrying with TCG" >&2
        rm -f "$pidfile"
        accel="tcg"
        accel_args=(-accel "tcg,thread=multi" -cpu max)
        launch_vm "$accel" "${accel_args[@]}"
    else
        exit 1
    fi
fi

cleanup() {
    if [[ -f "$pidfile" ]]; then
        pid="$(cat "$pidfile" 2>/dev/null || true)"
        if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null || true
            for _ in $(seq 1 20); do
                kill -0 "$pid" 2>/dev/null || break
                sleep 0.25
            done
            kill -9 "$pid" 2>/dev/null || true
        fi
    fi
}
trap cleanup EXIT

ssh_opts=(
    -i "$ssh_key"
    -p "$SSH_PORT"
    -o BatchMode=yes
    -o StrictHostKeyChecking=no
    -o UserKnownHostsFile=/dev/null
    -o ConnectTimeout=5
    -o ServerAliveInterval=10
    -o ServerAliveCountMax=3
)

scp_opts=(
    -i "$ssh_key"
    -P "$SSH_PORT"
    -o BatchMode=yes
    -o StrictHostKeyChecking=no
    -o UserKnownHostsFile=/dev/null
    -o ConnectTimeout=5
)

guest="flowerly@127.0.0.1"

wait_for_ssh() {
    local label="$1"
    local attempts="${2:-120}"
    echo "waiting for SSH ($label)"
    for _ in $(seq 1 "$attempts"); do
        if ssh "${ssh_opts[@]}" "$guest" true >/dev/null 2>&1; then
            return 0
        fi
        if [[ -f "$pidfile" ]]; then
            pid="$(cat "$pidfile" 2>/dev/null || true)"
            if [[ -n "$pid" ]] && ! kill -0 "$pid" 2>/dev/null; then
                echo "QEMU exited while waiting for SSH" >&2
                tail -n 200 "$qemu_log" >&2 || true
                return 1
            fi
        fi
        sleep 5
    done
    echo "timed out waiting for SSH ($label)" >&2
    tail -n 200 "$qemu_log" >&2 || true
    return 1
}

wait_for_ssh "initial boot"
ssh "${ssh_opts[@]}" "$guest" 'cloud-init status --wait'

echo "upgrading Arch and installing Flowerly build dependencies"
ssh "${ssh_opts[@]}" "$guest" \
    'sudo pacman -Syu --noconfirm --needed base-devel git just ripgrep rustup python'

echo "rebooting so tests execute on the kernel installed by the full upgrade"
ssh "${ssh_opts[@]}" "$guest" 'sudo systemctl reboot' || true

# Give sshd time to disappear before accepting a successful reconnect.
sleep 8
wait_for_ssh "post-upgrade reboot"

ssh "${ssh_opts[@]}" "$guest" \
    'rustup toolchain install stable --profile minimal --component rustfmt --component clippy && rustup default stable'

echo "exporting exact Git commit $TEST_SHA into the guest"
git -C "$ROOT" archive --format=tar "$TEST_SHA" | \
    gzip -c > "$WORK_DIR/flowerly_os.tar.gz"

scp "${scp_opts[@]}" "$WORK_DIR/flowerly_os.tar.gz" "$guest:flowerly_os.tar.gz"
ssh "${ssh_opts[@]}" "$guest" \
    'rm -rf "$HOME/flowerly_os" && mkdir -p "$HOME/flowerly_os" && tar -xzf "$HOME/flowerly_os.tar.gz" -C "$HOME/flowerly_os"'

# Recreate a tiny git identity so the guest report can still name the exact
# source commit even though we intentionally exported without .git.
printf '%s\n' "$TEST_SHA" | ssh "${ssh_opts[@]}" "$guest" \
    'cat > "$HOME/flowerly_os/.flowerly-tested-commit"'

echo "running complete Flowerly verification inside Arch VM"
set +e
ssh "${ssh_opts[@]}" "$guest" \
    'cd "$HOME/flowerly_os" && FLOWERLY_TEST_SHA="$(cat .flowerly-tested-commit)" bash testbed/evolution-world/arch-kernel/guest-verify.sh "$HOME/flowerly_os"' \
    2>&1 | tee "$REPORT_DIR/guest-execution.log"
guest_status=("${PIPESTATUS[@]}")
set -e

if ! scp "${scp_opts[@]}" \
    "$guest:flowerly_os/target/arch-kernel-report.txt" \
    "$REPORT_DIR/guest-report.txt"; then
    echo "Arch guest report could not be retrieved; preserved SSH and QEMU logs" >&2
    exit 1
fi
if [[ "${guest_status[0]}" -ne 0 || "${guest_status[1]}" -ne 0 ]]; then
    echo "Arch guest verification failed: ssh_exit=${guest_status[0]} tee_exit=${guest_status[1]}" >&2
    exit 1
fi

{
    echo "ref=$TEST_REF"
    echo "commit=$TEST_SHA"
    echo "arch_image_url=$ARCH_IMAGE_URL"
    echo "arch_image_sha256=$expected_sha"
    echo "qemu_accelerator=$accel"
    echo "vm_cpus=$VM_CPUS"
    echo "vm_memory_mb=$VM_MEMORY_MB"
} >"$REPORT_DIR/host-metadata.txt"

echo "requesting clean guest shutdown"
ssh "${ssh_opts[@]}" "$guest" 'sudo systemctl poweroff' || true
if [[ -f "$pidfile" ]]; then
    pid="$(cat "$pidfile" 2>/dev/null || true)"
    for _ in $(seq 1 60); do
        [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null || break
        sleep 1
    done
fi

echo "Arch kernel system test passed for $TEST_REF @ $TEST_SHA"
