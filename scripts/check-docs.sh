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
    web-docs/docs/architecture.md web-docs/docs/language-policy.md web-docs/docs/flow-model.md
    web-docs/docs/goal-model.md web-docs/docs/session-model.md web-docs/docs/generations.md
    web-docs/docs/evolution.md web-docs/docs/provenance.md web-docs/docs/capabilities.md
    web-docs/docs/user-model.md web-docs/docs/threat-model.md web-docs/docs/build.md web-docs/docs/releases.md
    web-docs/docs/vision.md web-docs/docs/roadmap.md web-docs/docs/decisions.md web-docs/docs/journal.md
    web-docs/docs/image-model.md web-docs/docs/review-implementation-plan-2026-10-12.md
)

required_adrs=(
    web-docs/docs/adr/0001-rust-as-primary-system-language.md
    web-docs/docs/adr/0002-go-for-distributed-orchestration.md
    web-docs/docs/adr/0003-python-for-experimentation.md
    web-docs/docs/adr/0004-upstream-linux-kernel-policy.md
    web-docs/docs/adr/0005-sqlite-as-initial-state-store.md
    web-docs/docs/adr/0006-systemd-service-supervision.md
    web-docs/docs/adr/0007-kvm-evolution-world.md
    web-docs/docs/adr/0008-flows-as-first-class-os-objects.md
    web-docs/docs/adr/0009-project-name-flowerly-os.md
    web-docs/docs/adr/0010-unix-socket-json-ipc.md
    web-docs/docs/adr/0011-shared-local-state-database.md
    web-docs/docs/adr/0012-agenticctl-control-plane.md
    web-docs/docs/adr/0013-process-boundaries-require-justification.md
    web-docs/docs/adr/0014-linux-authority-is-the-enforcement-substrate.md
    web-docs/docs/adr/0015-arch-linux-userspace-substrate.md
)

required_rfcs=(
    web-docs/docs/rfcs/index.md
    web-docs/docs/rfcs/0001-intent-as-first-class-domain.md
    web-docs/docs/rfcs/0002-flow-state-decomposition.md
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