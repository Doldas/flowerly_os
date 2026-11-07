#!/usr/bin/env bash
# Fail fast on the offline checks in the GitHub Actions docs job.
# Run before opening or updating any PR. This does not replace CI.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

python3 scripts/test-release-gates.py
python3 scripts/test-branch-forward.py
python3 scripts/test-project-sync-workflows.py
python3 scripts/test-project-board-reconcile.py
python3 scripts/test-wait-release-promotion.py
python3 scripts/test-layer-15-boundaries.py
python3 scripts/test-audit-storage-schema.py
python3 scripts/audit-storage-schema.py > /dev/null
python3 scripts/test-audit-legacy-sqlite-backup.py
python3 scripts/test-runtime-topology.py
python3 scripts/test-shipped-ipc-admission.py
python3 scripts/test-coreonly-unit-policy.py
python3 scripts/test-pty-ipc-framing.py
python3 scripts/test-offline-memory-prototype.py
python3 scripts/test-build-task-parity.py
python3 scripts/test-sqlite-operational-health.py
bash scripts/check-image-inputs.sh
python3 scripts/test-build-arch-rootfs.py
python3 scripts/test-layer-15-sqlite-contracts.py
python3 scripts/test-install-flowerly-rootfs.py
python3 scripts/test-prepare-arch-boot.py
python3 scripts/test-boot-ipc-health.py
python3 scripts/test-build-arch-root-volume.py
bash -n scripts/build-flowerly-ab-disk.sh
python3 scripts/test-build-flowerly-ab-disk.py
python3 scripts/test-verify-ab-slot-layout.py
# All Generation 0.0.5 offline contracts present in the checkout must pass.
for contract in scripts/test-generation-005-*.py; do
    [[ -f "$contract" ]] || continue
    python3 "$contract"
done
python3 scripts/test-probe-arch-qemu-boot.py
python3 scripts/test-build-arch-initrd.py
bash -n scripts/test-arch-vm-disk.sh
bash scripts/test-arch-vm-disk.sh
bash scripts/test-arch-vm-stage-timing.sh
python3 scripts/test-arch-vm-failure-classify.py
bash -n scripts/remote-provider-arch-kvm-acceptance.sh
python3 scripts/test-remote-provider-kvm-harness.py
python3 scripts/test-remote-provider-egress-ownership.py
python3 scripts/test-cluster-peer-admission.py
python3 scripts/test-arch-image-verify.py
python3 scripts/test-host-process-authority.py

echo "Offline docs preflight passed. CI docs also builds the site and checks Markdown."
