#!/usr/bin/env bash
# Fail fast on the offline checks in the GitHub Actions docs job.
# Run before opening or updating any PR. This does not replace CI.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

python3 scripts/test-release-gates.py
python3 scripts/test-branch-forward.py
python3 scripts/test-build-task-parity.py
bash scripts/check-image-inputs.sh
python3 scripts/test-build-arch-rootfs.py
python3 scripts/test-install-flowerly-rootfs.py
python3 scripts/test-prepare-arch-boot.py
python3 scripts/test-boot-ipc-health.py
python3 scripts/test-build-arch-root-volume.py
python3 scripts/test-probe-arch-qemu-boot.py
python3 scripts/test-build-arch-initrd.py
bash -n scripts/test-arch-vm-disk.sh
bash scripts/test-arch-vm-disk.sh

echo "Offline docs preflight passed. CI docs also builds the site and checks Markdown."
