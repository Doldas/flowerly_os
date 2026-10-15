#!/usr/bin/env bash
# Sourced by arch-kernel-vm-test.sh; deliberately no side effects on import.

flowerly_arch_vm_validate_disk_gb() {
    local size="${1:-}"
    # Only decimal GiB values, bounded for a disposable sparse test VM.
    if [[ ! "$size" =~ ^[1-9][0-9]{1,2}$ ]] || \
        (( 10#$size < 24 || 10#$size > 256 )); then
        echo "ARCH_VM_DISK_GB must be a decimal integer between 24 and 256 (GiB); got '${size}'" >&2
        return 1
    fi
}

flowerly_arch_vm_resize_overlay() {
    local overlay="$1"
    local size_gb="$2"
    flowerly_arch_vm_validate_disk_gb "$size_gb" || return 1
    # Resize ONLY the new disposable overlay, never the cached SHA-verified base.
    # QEMU uses sparse qcow2 allocation; host blocks grow as guest data is written.
    qemu-img resize -f qcow2 "$overlay" "${size_gb}G"
}
