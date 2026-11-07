# 0.0.5 Generation proof 6: Generationsidentitet

Tracking: [#326](https://github.com/Doldas/flowerly_os/issues/326),
parent [#9](https://github.com/Doldas/flowerly_os/issues/9).

## Implementation

An independent offline contract and its positive and fail-closed negative
unit tests live under `scripts/generation-005-*.py` and
`scripts/test-generation-005-*.py`. The Docs workflow and local preflight
execute all present tests in a clean checkout. This increment is a partial
prototype, **not an installed kernel, EFI, systemd or daemon feature**.

## Threat model and test boundary

Reject missing, ambiguous, malformed or contradictory evidence rather than
interpreting it as a passing boot or safe root transition. Do not mutate the
current root filesystem, ESP, user-data partition or persistent sqlite
from this offline contract. A successful test cannot substitute for actual
firmware boot, cgroup/IPC health, power-loss recovery or manual approval.

## Outstanding acceptance

- [ ] Verify the exact latest PR head with Docs, Rust, security and CodeQL.
- [ ] Integrate under the reviewed A/B slot ownership design, if applicable.
- [ ] Prove behavior on a real Flowerly-built OVMF/KVM disk, not stock Arch.
- [ ] Preserve user data, boot identity and authenticated actor context.
- [ ] Review the complete S4-S8 plan and obtain explicit maintainer approval.

Read [0.0.5 Generations plan](../release-0.0.5-generations-plan.md) and
[step 1 A/B boot plan](../release-0.0.5-step1-ab-boot-rollback-plan.md).
