# 0.0.5 Generations — readiness against the roadmap (2026-11-17)

Status: **ENGINEERING COMPLETE ON `release/2`; NOT RELEASED.** Nothing here
tags, promotes or publishes. The remaining gates are the exact-head push CI and
the owner's decisions. Tracking: [#9](https://github.com/Doldas/flowerly_os/issues/9),
plan: [release-0.0.5-generations-plan.md](release-0.0.5-generations-plan.md).

## Roadmap checklist (#9) against evidence

| #9 item | Evidence |
| --- | --- |
| typed Generation manifest; create/list/inspect/test/promote/activate/rollback | ADR-0023 G1–G4b; transitions enforced in the database |
| source commit, parent, reason, provenance; activation history | G1/G2; history rows with actor, reason, previous Generation |
| no public status setter; DB-level protection of the stable invariant | G2 (typed transitions only), G3 witness resolution |
| Arch package set: exact name/version/architecture | `distro/base-image/arch/inputs.manifest.json`: 157 packages, snapshot 2026/11/09, manifest `8689cd41…` |
| acquisition identity and artifact hashes; signatures | `freeze-arch-snapshot.py`, `trust-policy.json`, `verify-cache --keyring` (314 artifacts) |
| kernel artifact/config identity | `verify-binding`: frozen `vmlinuz` `b1e9eaf5…`, embedded config `e8f340e4…`, gcc 16.2.1 20260810 |
| Flowerly source/output hashes | baked identity v2: `source_commit`, rootfs/kernel/initramfs hashes, `input_manifest_sha256` |
| pinned/recorded build toolchain identity | `build-tooling.json` (container by digest, tools from the same snapshot) |
| schema-version guard; coherent backup; recovery | G3 schema window, G4b `VACUUM INTO` backup, G5 restore drill |
| rollback to a known previous state; history survives reboot | G5 on a real OVMF/KVM guest: 16/16, twice (`eb60060` and, on the pinned rootfs, `b05e3be`) |
| minimal bootable fixture; exact identity in the manifest | S7 matrix (13 scenarios), `build-g5-test-image.sh`, `verify-rootfs` (157/0/0/0) |
| #48 substrate decision and proof | ADR-0019 accepted; S6 slot tool; S7 evidence |
| #49 pinned acquisition, restore after upstream change | Gate B review; drill: the mirror moved (26 packages) and the cache still verified |
| #51 Flowerly-built Arch boot in KVM | closed with evidence |

## Release gates (`releases.md`)

| Gate | State |
| --- | --- |
| CI green | PR checks green on every merged PR; **exact-head push CI on `release/2` pending** (run for `5adba0c` was still in progress; a merge cancels the previous push run) |
| real-Arch kernel validation | part of that push run |
| migration path | schema window and the G5 downgrade/restore drill |
| boot test, rollback test | G5 and S7 evidence |
| known data corruption | none known |
| release notes | `CHANGELOG.md` `Unreleased` holds the entries; the 0.0.5 section is written when tagging |

## What remains, and who decides

1. **Green exact-head push CI** on the final `release/2` head, then re-run the
   promotion PR's "Verify release promotion exact-SHA Arch acceptance" (#388).
   No merges to `release/2` while it runs.
2. **Owner:** promotion to `master`, the tag and the GitHub Release; none is done
   by an agent.
3. **Owner:** the open Scorecard alerts (posture checks, e.g. Vulnerabilities,
   Maintained, CodeReview, BranchProtection, Fuzzing, Pinned dependencies; no
   open Dependabot alerts) are tracked in #187.
4. **Release notes** must say what is verified and what is not (below).
5. Workspace version still reports `0.0.1` (known since 0.0.4); decide whether
   0.0.5 bumps it.
6. Housekeeping the owner can close: #359 and #364 (python-quality passes on
   push and PRs); #285 (Arch VM speed, PR #399) is not a release blocker.

## What the release notes must not overstate

- Generation activation is verified on the **test image** only. The slot broker
  and its units are installed by the explicit test-profile script; the shipped
  image does not carry them, and block-device/live-ESP writes by `flowerlyd` do
  not exist. `RELEASE_ACTIVATION_ENABLED` is on, so on a production host an
  activation request reaches no broker and is refused as unavailable.
- Package inputs are frozen and verified, not bit-for-bit reproducible images.
  The manifest is v1; v2 (carrying the tooling) comes at the next re-freeze.
- The initramfs of the Gate D images is a generic mkosi initrd, not part of the
  frozen set.
- No cryptographic proof of the KVM runs: serial logs are self-consistent
  evidence, with hashes recorded.
- Hypervisor provisioning and bootable image builds remain unavailable
  (`AGENTS.md`); the AI-compute work (#263, RFC 0004) is not 0.0.5 scope.
