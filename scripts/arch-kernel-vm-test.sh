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
# QCOW2 is sparse: this is virtual guest capacity, not host preallocation.
VM_DISK_GB="${ARCH_VM_DISK_GB:-32}"
source "$ROOT/scripts/lib/arch-vm-disk.sh"
flowerly_arch_vm_validate_disk_gb "$VM_DISK_GB"


mkdir -p "$CACHE_DIR"
rm -rf "$REPORT_DIR"
mkdir -p "$REPORT_DIR"

# #184: numeric host facts at the start, so the end of the run (or the next
# run, after a host reset killed this one) can tell what happened.
oom_kill_count() {
    awk '$1 == "oom_kill" { print $2; found = 1 } END { if (!found) print 0 }' \
        /sys/fs/cgroup/memory.events 2>/dev/null || echo 0
}
boot_id_start="$(cat /proc/sys/kernel/random/boot_id 2>/dev/null || true)"
oom_kill_start="$(oom_kill_count)"
stage="before-vm"
guest_ssh_status=""
{
    printf 'test_sha=%s\n' "$TEST_SHA"
    printf 'run_started_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'host_boot_id=%s\n' "$boot_id_start"
    printf 'host_boot_time_utc=%s\n' "$(date -u -d "@$(( $(date +%s) - $(cut -d. -f1 /proc/uptime) ))" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || true)"
    printf 'host_cpus=%s\n' "$(nproc 2>/dev/null || true)"
    grep -E '^(MemTotal|MemAvailable|SwapTotal|SwapFree):' /proc/meminfo 2>/dev/null || true
    printf 'runner_cgroup_oom_kill=%s\n' "$oom_kill_start"
    printf 'kvm_device=%s\n' "$([[ -c /dev/kvm && -r /dev/kvm && -w /dev/kvm ]] && echo usable || echo unavailable)"
} > "$REPORT_DIR/host-resource-start.txt" 2>/dev/null || true
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
flowerly_arch_vm_resize_overlay "$overlay" "$VM_DISK_GB"
echo "Arch guest sparse overlay configured with ${VM_DISK_GB} GiB virtual disk"

ssh_key="$WORK_DIR/id_ed25519"
rm -f "$ssh_key" "$ssh_key.pub"
ssh-keygen -q -t ed25519 -N "" -f "$ssh_key" -C "flowerly-arch-kernel-ci"
public_key="$(cat "$ssh_key.pub")"

cat >"$WORK_DIR/user-data" <<EOF
#cloud-config
# Expand the existing root partition and its filesystem on the first boot.
# Both are required: increasing the QCOW2 virtual size alone is insufficient.
growpart:
  mode: auto
  devices: ['/']
resize_rootfs: true
# The CI login is not named flowerly: that is the service account the
# guest's systemd acceptance creates from the shipped sysusers file (#225).
users:
  - name: flowerly-ci
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

resource_monitor_pid=""
cleanup() {
    # First statement: anything earlier would overwrite the script's status.
    local exit_status=$?
    # Stop the background monitor before collecting final resource counters.
    if [[ -n "$resource_monitor_pid" ]]; then
        kill "$resource_monitor_pid" 2>/dev/null || true
        wait "$resource_monitor_pid" 2>/dev/null || true
    fi
    # Preserve non-secret host resource evidence even if guest SSH/report retrieval fails.
    # Do not read kernel journals or guest credentials into CI artifacts.
    local qemu_state=no_pidfile guest_reachable=unknown
    if [[ -f "$pidfile" ]]; then
        qemu_state=absent
        local running_pid
        running_pid="$(cat "$pidfile" 2>/dev/null || true)"
        if [[ "$running_pid" =~ ^[0-9]+$ ]] && kill -0 "$running_pid" 2>/dev/null; then
            qemu_state=alive
        fi
    fi
    if [[ "$exit_status" -ne 0 && "$qemu_state" == alive && -n "${guest:-}" ]]; then
        guest_reachable=no
        if timeout 20 ssh "${ssh_opts[@]}" "$guest" true >/dev/null 2>&1; then
            guest_reachable=yes
        fi
    fi
    {
        printf 'test_sha=%s\n' "$TEST_SHA"
        printf 'exit_status=%s\n' "$exit_status"
        printf 'qemu_accelerator=%s\n' "$accel"
        printf 'vm_memory_mb=%s\n' "$VM_MEMORY_MB"
        if [[ -f "$pidfile" ]]; then
            local qemu_pid
            qemu_pid="$(cat "$pidfile" 2>/dev/null || true)"
            if [[ "$qemu_pid" =~ ^[0-9]+$ ]] && kill -0 "$qemu_pid" 2>/dev/null; then
                printf 'qemu_process=alive\n'
                grep -E '^(State|VmRSS|VmPeak):' "/proc/$qemu_pid/status" 2>/dev/null || true
            else
                printf 'qemu_process=absent\n'
            fi
        else
            printf 'qemu_process=no_pidfile\n'
        fi
        grep -E '^(MemTotal|MemAvailable|SwapTotal|SwapFree):' /proc/meminfo 2>/dev/null || true
        if [[ -r /sys/fs/cgroup/memory.events ]]; then
            echo 'host_cgroup_memory_events:'
            cat /sys/fs/cgroup/memory.events
        fi
        printf 'failed_stage=%s\n' "$stage"
    } > "$REPORT_DIR/host-resource-exit.txt" 2>/dev/null || true
    python3 "$ROOT/scripts/arch-vm-failure-classify.py" --report-dir "$REPORT_DIR" \
        --exit-status "$exit_status" --stage "$stage" \
        ${guest_ssh_status:+--ssh-status "$guest_ssh_status"} \
        --qemu "$qemu_state" --guest-reachable "$guest_reachable" \
        --oom-kill-start "$oom_kill_start" --oom-kill-end "$(oom_kill_count)" \
        --boot-id-start "$boot_id_start" \
        --boot-id-end "$(cat /proc/sys/kernel/random/boot_id 2>/dev/null || true)" || true
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
stage="boot"

# Capture resource pressure *before* any guest disconnection, not only at exit.
# The collector is observational and never changes a release acceptance result.
python3 "$ROOT/scripts/arch-vm-resource-monitor.py" \
    --pidfile "$pidfile" \
    --output "$REPORT_DIR/host-resource-timeline.csv" \
    --interval 10 --max-samples 1000 >/dev/null 2>&1 &
resource_monitor_pid=$!

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

guest="flowerly-ci@127.0.0.1"

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
stage="upgrade"

# Fail before a large pacman transaction if cloud-init did not expand the
# partition/filesystem. Save this diagnostic even if the guest fails preflight.
echo "verifying expanded Arch guest root filesystem before pacman"
if ! ssh "${ssh_opts[@]}" "$guest" 'bash -s -- 16 8' \
    < "$ROOT/scripts/arch-vm-guest-disk-preflight.sh" \
    2>&1 | tee "$REPORT_DIR/guest-disk-before-upgrade.txt"; then
    echo "Arch guest root filesystem did not grow; see $REPORT_DIR/guest-disk-before-upgrade.txt" >&2
    exit 1
fi

echo "upgrading Arch and installing Flowerly build dependencies"
ssh "${ssh_opts[@]}" "$guest" \
    'sudo pacman -Syu --noconfirm --needed base-devel git just ripgrep rustup python'

echo "rebooting so tests execute on the kernel installed by the full upgrade"
stage="reboot"
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
stage="verify"
set +e
ssh "${ssh_opts[@]}" "$guest" \
    'cd "$HOME/flowerly_os" && FLOWERLY_TEST_SHA="$(cat .flowerly-tested-commit)" bash testbed/evolution-world/arch-kernel/guest-verify.sh "$HOME/flowerly_os"' \
    2>&1 | tee "$REPORT_DIR/guest-execution.log"
guest_status=("${PIPESTATUS[@]}")
guest_ssh_status="${guest_status[0]}"
stage="report"
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
    echo "vm_disk_gb=$VM_DISK_GB"
    echo "vm_root_minimum_gb=16"
    echo "vm_free_minimum_gb=8"
} >"$REPORT_DIR/host-metadata.txt"

echo "requesting clean guest shutdown"
stage="shutdown"
ssh "${ssh_opts[@]}" "$guest" 'sudo systemctl poweroff' || true
if [[ -f "$pidfile" ]]; then
    pid="$(cat "$pidfile" 2>/dev/null || true)"
    for _ in $(seq 1 60); do
        [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null || break
        sleep 1
    done
fi

echo "Arch kernel system test passed for $TEST_REF @ $TEST_SHA"
