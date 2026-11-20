# 0.0.5 Generations — readiness against the roadmap (2026-11-17)

Status: **Generation lifecycle verified on the 0.0.5 test fixture.**
The milestone remains open for protected release CI evidence. Tracking: [#9](https://github.com/Doldas/flowerly_os/issues/9),
plan: [release-0.0.5-generations-plan.md](release-0.0.5-generations-plan.md).

## Roadmap checklist (#9) against evidence

| #9 item | Evidence |
| --- | --- |
| typed Generation manifest; create/list/inspect/test/promote/activate/rollback | ADR-0023 G1–G4b; transitions enforced in the database |
| source commit, parent, reason, provenance; activation history | G1/G2; history rows with actor, reason, previous Generation |
| no public status setter; DB-level protection of the stable invariant | G2 (typed transitions only), G3 witness resolution |
| Arch package set: exact name/version/architecture | `distro/base-image/arch/inputs.manifest.json`: 157 packages, snapshot 2026/11/09, manifest `8689cd41…` |
| acquisition identity and artifact hashes; signatures | `freeze-arch-snapshot.py`, `trust-policy.json`, `verify-cache --keyring` (314 artifacts) |
| kernel source/config/artifact identity | `verify-binding`: signed Arch kernel package `7.2.14.arch1-1`, `vmlinuz` `b1e9eaf5…`, embedded config `e8f340e4…`, gcc 16.2.1 20260810. The signed package build metadata binds the [tagged Arch recipe and upstream source](reviews/0049-gate-b-frozen-package-set-2026-11-16.md#upstream-kernel-source-identity-2026-11-19). |
| Flowerly source/output hashes | Baked identity v2 records `source_commit` and `input_manifest_sha256`. Generation activation verifies the rootfs, kernel and initramfs hashes against the staged slot manifest. |
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
| CI green | Baseline `fd5e38656da6dad0c8578371d6cae23c6dd061c7`: [push CI](https://github.com/Doldas/flowerly_os/actions/runs/44124162315), [security](https://github.com/Doldas/flowerly_os/actions/runs/44124162400), [CodeQL](https://github.com/Doldas/flowerly_os/actions/runs/44124162314) |
| real-Arch kernel validation | Push artifact confirms Arch kernel `7.3.4-arch1-2`, accelerator TCG, and completion at the exact baseline SHA. The separate Flowerly-built fixture acceptance used KVM. |
| migration path | schema window and the G5 downgrade/restore drill |
| boot test, rollback test | G5 and S7 evidence |
| known data corruption | none known |
| release notes | `CHANGELOG.md` records the changes under Unreleased |

## Remaining milestone work

- #48, #359 and #364 are closed. S7 covers the substrate decision and its
  power-loss matrix. The full-tree Ruff check passed on all 150 Python files
  in the baseline. Enabling the checked-in `python-quality` rule in GitHub
  remains an administrator action.
- #49 has a repeatable signed package acquisition, a verified pinned
  build, and measured rootfs and KVM resources. The
  [Gate B review](reviews/0049-gate-b-frozen-package-set-2026-11-16.md#repeat-acquisition-and-kvm-resource-measurements-2026-11-19)
  records the evidence. Manifest v2 belongs to the next re-freeze per ADR-0020.
- #285 has a matched local KVM cold/warm comparison and two failed-cache
  drills. Both protected push attempts for `d653371` lost the runner during
  Arch acceptance. See [the VM timing notes](arch-vm-stage-timings.md) and
  [runner interruption details](arch-vm-resource-timeline.md#release-push-runner-interruption-2026-11-19).
- #9 tracks the Generation milestone until the remaining evidence is in.
- After the final merge, the resulting `release/2` SHA needs fresh push CI,
  including the executed Arch step, security and CodeQL. The owner controls
  promotion in [#388](https://github.com/Doldas/flowerly_os/pull/388), the
  version bump decision, tag and publication. Scorecard dispositions remain
  tracked in [#187](https://github.com/Doldas/flowerly_os/issues/187).

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
