#!/usr/bin/env bash
# Verify the documentation required by the repository spec exists and is
# non-empty. Used by the docs CI job.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

required_root=(
    README.md VISION.md ARCHITECTURE.md ROADMAP.md CONTRIBUTING.md
    SECURITY.md CODE_OF_CONDUCT.md DECISIONS.md JOURNAL.md CHANGELOG.md
    LICENSE justfile Makefile rust-toolchain.toml Cargo.toml
)

required_docs=(
    docs/ARCHITECTURE.md docs/LANGUAGE_POLICY.md docs/FLOW_MODEL.md
    docs/GOAL_MODEL.md docs/SESSION_MODEL.md docs/GENERATIONS.md
    docs/EVOLUTION.md docs/PROVENANCE.md docs/CAPABILITIES.md
    docs/USER_MODEL.md docs/THREAT_MODEL.md docs/BUILD.md docs/RELEASES.md
)

required_adrs=(
    adr/0001-rust-as-primary-system-language.md
    adr/0002-go-for-distributed-orchestration.md
    adr/0003-python-for-experimentation.md
    adr/0004-upstream-linux-kernel-policy.md
    adr/0005-sqlite-as-initial-state-store.md
    adr/0006-systemd-service-supervision.md
    adr/0007-kvm-evolution-world.md
    adr/0008-flows-as-first-class-os-objects.md
    adr/0009-project-name-flowerly-os.md
)

failures=0

for f in "${required_root[@]}" "${required_docs[@]}" "${required_adrs[@]}"; do
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