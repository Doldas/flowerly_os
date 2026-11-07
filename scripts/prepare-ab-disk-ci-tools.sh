#!/usr/bin/env bash
# Provision the real disk-builder dependencies on the trusted release runner.
# Do not fake or skip the GPT/disk-image acceptance tests when a tool is absent.
set -euo pipefail
export PATH="$PATH:/usr/sbin:/sbin"
required=(sfdisk mkfs.vfat mkfs.ext4 dumpe2fs mmd mcopy truncate dd sha256sum python3)
missing=()
for tool in "${required[@]}"; do
    command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
done
if (( ${#missing[@]} )); then
    echo "A/B CI host missing real disk tools: ${missing[*]}"
    command -v apt-get >/dev/null 2>&1 || {
        echo "A/B CI host needs fdisk, dosfstools, mtools, e2fsprogs (apt-get unavailable)" >&2
        exit 1
    }
    elevation=()
    if [[ $(id -u) -ne 0 ]]; then
        command -v sudo >/dev/null 2>&1 || {
            echo "A/B CI host needs root or non-interactive sudo to install disk tools" >&2
            exit 1
        }
        elevation=(sudo -n)
    fi
    export DEBIAN_FRONTEND=noninteractive
    "${elevation[@]}" apt-get update
    "${elevation[@]}" apt-get install -y --no-install-recommends fdisk dosfstools mtools e2fsprogs
fi
for tool in "${required[@]}"; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "A/B CI host still missing required tool: $tool" >&2
        exit 1
    }
done
echo "A/B disk CI prerequisites verified (real GPT, FAT and ext4 tools)"
