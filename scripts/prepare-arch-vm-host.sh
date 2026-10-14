#!/usr/bin/env bash
set -euo pipefail

required_tools=(curl qemu-img qemu-system-x86_64 cloud-localds ssh scp ssh-keygen tar)
ovmf_paths=(
  /usr/share/OVMF/OVMF_CODE_4M.fd
  /usr/share/OVMF/OVMF_CODE.fd
  /usr/share/edk2/ovmf/OVMF_CODE.fd
  /usr/share/edk2/x64/OVMF_CODE.4m.fd
)

host_ready() {
  for tool in "${required_tools[@]}"; do
    command -v "$tool" >/dev/null 2>&1 || return 1
  done
  for candidate in "${ovmf_paths[@]}"; do
    [[ -f "$candidate" ]] && return 0
  done
  return 1
}

if ! host_ready; then
  if ! command -v apt-get >/dev/null 2>&1; then
    printf '%s\n' "Trusted runner needs QEMU, cloud image tools and OVMF; automatic setup requires apt-get" >&2
    exit 1
  fi
  elevation=()
  if [[ $(id -u) -ne 0 ]]; then
    elevation=(sudo -n)
  fi
  "${elevation[@]}" apt-get update
  "${elevation[@]}" apt-get install -y \
    qemu-system-x86 qemu-utils ovmf cloud-image-utils openssh-client curl
fi

if ! host_ready; then
  printf '%s\n' "Trusted runner still lacks QEMU, cloud-init or OVMF after prerequisite installation" >&2
  exit 1
fi

printf '%s\n' "Arch VM host dependencies available; guest must still prove actual delegated kernel containment"
