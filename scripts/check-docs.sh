#!/usr/bin/env bash
# Verify the documentation required by the repository spec exists and is
# non-empty. Used by the docs CI job.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

required_root=(
    README.md CONTRIBUTING.md SECURITY.md CODE_OF_CONDUCT.md CHANGELOG.md
    LICENSE justfile Makefile rust-toolchain.toml Cargo.toml
)

required_docs=(
    docs/architecture.md docs/language-policy.md docs/flow-model.md
    docs/goal-model.md docs/session-model.md docs/generations.md
    docs/evolution.md docs/provenance.md docs/capabilities.md
    docs/user-model.md docs/threat-model.md docs/build.md docs/releases.md
    docs/vision.md docs/roadmap.md docs/decisions.md docs/journal.md
    docs/image-model.md docs/review-implementation-plan-2026-10-12.md
)

required_adrs=(
    docs/adr/0001-rust-as-primary-system-language.md
    docs/adr/0002-go-for-distributed-orchestration.md
    docs/adr/0003-python-for-experimentation.md
    docs/adr/0004-upstream-linux-kernel-policy.md
    docs/adr/0005-sqlite-as-initial-state-store.md
    docs/adr/0006-systemd-service-supervision.md
    docs/adr/0007-kvm-evolution-world.md
    docs/adr/0008-flows-as-first-class-os-objects.md
    docs/adr/0009-project-name-flowerly-os.md
    docs/adr/0010-unix-socket-json-ipc.md
    docs/adr/0011-shared-local-state-database.md
    docs/adr/0012-agenticctl-control-plane.md
    docs/adr/0013-process-boundaries-require-justification.md
    docs/adr/0014-linux-authority-is-the-enforcement-substrate.md
    docs/adr/0015-arch-linux-userspace-substrate.md
)

required_rfcs=(
    docs/rfcs/index.md
    docs/rfcs/0001-intent-as-first-class-domain.md
    docs/rfcs/0002-flow-state-decomposition.md
)

failures=0

for f in "${required_root[@]}" "${required_docs[@]}" "${required_adrs[@]}" "${required_rfcs[@]}"; do
    if [[ ! -s "$ROOT/$f" ]]; then
        echo "missing or empty: $f"
        failures=$((failures + 1))
    fi
done

if ! rg -q '^missing ' "$ROOT/justfile"; then
    : # no-op; justfile presence is checked above
fi

if [[ $failures -ne 0 ]]; then
    echo "documentation check failed: $failures missing file(s)"
    exit 1
fi

echo "documentation check ok: all required files present"