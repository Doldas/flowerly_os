# Flowerly OS 0.0.5 — Generations implementation and acceptance plan

Status: **PLANNING / NOT RELEASE-CERTIFIED**. Started 2026-10-29.
Tracking: [#9](https://github.com/Doldas/flowerly_os/issues/9).
Dependencies: [#48](https://github.com/Doldas/flowerly_os/issues/48),
[#49](https://github.com/Doldas/flowerly_os/issues/49),
[#51](https://github.com/Doldas/flowerly_os/issues/51).

## Baseline and branch discipline

- v0.0.4 is published at annotated tag `v0.0.4`, SHA
  `b13ce02b0eb1fd88471b17b8dc60a7ea962c630c`;
  [release workflow 40753771279](https://github.com/Doldas/flowerly_os/actions/runs/40753771279)
  passed the real Arch VM and release build.
- At kickoff, `master` was `5baf1dd663edad988f48243eed3ae56ec4408f5c`
  and `release/2` was `26a4c054c80545ecccf254da4b5f67ab5cd02523`.
  **They are not the same SHA**. Reconcile the released/master ancestry into
  the active integration branch using a reviewed PR and the existing
  branch-forward procedure before any runtime work. Never force-push or
  bypass branch protection.
- Scope all changes to small feature/docs branches targeting the currently
  authorized release integration branch. A new `release/3` must be created
  only after verifying the branch-forward policy, base SHA, CI and maintainer
  intention; do not assume a branch name from the version number.
- No automatic promotion to `master`; no `v0.0.5` tag or publication without
  explicit owner approval and exact-head acceptance.

## Threat model and invariants

1. **System vs user data:** snapshot/rollback affects system state only;
   persistent user data and agentic memory must not be rolled back or silently
   discarded. Database/schema compatibility must fail closed on downgrade.
2. **No metadata-only rollback claim:** an active Generation must identify the
   actual booted rootfs/kernel/initramfs and installed package artifacts.
   The current Generation API is metadata-only; distinguish it explicitly.
3. **No unpinned pacman mutation:** rolling Arch mirrors cannot directly mutate
   the stable root. Candidate inputs must be exact, trusted, hash-verified and
   retained or recoverable from an immutable archive.
4. **Power loss and rollback:** interruption at every write/activate/reboot
   boundary must either retain the old bootable generation or recover to a
   verifiably known one. Never make a candidate stable merely by setting a DB
   status.
5. **Trust and authorization:** kernel peer identity, explicit admin permission,
   provenance, immutable evidence and safe IPC retry semantics apply to every
   destructive transition. No AI-generated preference can grant OS authority.

## Ordered workstreams

### Gate A — Real snapshot and rollback substrate (#48)

- Compare btrfs snapshots, LVM snapshots and image/OSTree-like deployments in
  an ADR; include Arch bootloader integration, space/GC, recovery and rollback
  ownership.
- Choose one with measurable failure semantics. Prototype actual A → B →
  rollback A in a bootable VM, with separate persistent user data.
- Inject interrupted activation and failed boot; capture booted root identity,
  disk/space measurements and recovery evidence.
- **Exit:** reviewer accepts ADR and reproducible real-VM evidence. No claim
  of complete Generation rollback before this gate.

### Gate B — Pinned package and build identity (#49)

- Define a canonical, versioned manifest recording each package name, version,
  architecture, repository snapshot/acquisition URL, signature/trust evidence
  and SHA256; include kernel source/config/artifact, toolchain, Flowerly
  commit and output hashes.
- Select archive/snapshot acquisition and retention/cache policy. Prove
  byte-identical *inputs* can be fetched after rolling mirrors change;
  distinguish input reproducibility from bit-for-bit deterministic builds.
- Fail closed on missing, tampered or untrusted packages. Test offline cache
  replay and corrupted manifest/artifact rejection.
- **Exit:** exact resolved package set and provenance independently verified.
- **Progress (2026-11-01):** the v1 package manifest format, canonical
  digest and offline cache verification are implemented in
  `distro-builder manifest check|verify-cache` (see ADR-0020 implementation
  status). Missing and tampered archives/signatures fail closed, and
  `--keyring` checks each detached signature with `gpgv` against the key the
  manifest names. Still open: the trust policy (which keyring, expiry on
  replay), repository database resolution, a real frozen package set and the
  mirror-change demonstration; kernel/toolchain/output identity is not yet in
  the schema.

### Gate C — Minimal bootable Flowerly candidate (#51)

- Build a minimal Arch rootfs fixture with Flowerly binaries and units,
  pinned kernel/initramfs/boot metadata and a stable identity in the manifest.
- Boot the **built candidate image** in KVM; do not substitute testing a
  stock Arch guest. Prove services, package set, rootfs identity and isolated
  persistent data after reboot.
- Capture QEMU commands, exact source SHA, serial logs, artifact hashes and
  host/guest exit codes.
- **Exit:** deterministic CI-compatible image build and real guest boot,
  without pretending this is the distribution-grade 0.0.9 installer.

### Gate D — Generation lifecycle and durable activation (#9)

**Design (accepted 2026-11-09):** [ADR-0023](adr/0023-generation-lifecycle-and-durable-activation.md)
records the verified gaps in today's code (no transition check, no activation
history, `STABLE` set by request, rollback unsafe across a schema change), the
owner decisions Q1–Q6, the requirements R1–R5 and the work breakdown G1–G5.

**G1 storage:** migration 0015, typed transitions and activation history. The
database refuses every other route to `STABLE`, so `generation.activate` now
always refuses (`conflict`) until G2–G5 replace it. Evidence for `TESTED` is an
Experiment about that Generation; nothing in production creates Experiments
yet, so G2 had to settle how evidence is recorded. Crash injection in G1 covers
transaction failure and restart; killing the process at every point of witness
resolution belongs to G3 (R2).

**G2 IPC:** `generation.identity|evidence|test|reject|revoke|history`, plus
`activate` and `rollback`, which validate and refuse until Gate D is verified.
All are Admin only, attributed to the kernel-attested caller, and available in
`agenticctl`. Next: G3 (witness resolution and the schema window).

**G3 resolution and schema window:** the guest's chained ledger v2 with health
records, the verifying reader, resolution at start-up and through
`generation.resolve`, and migration 16 (`min_reader` / `schema_compat`), with
a real-binary compatibility test against the recorded older commit in CI.
Next: G4 (slot broker; first move the compatibility baseline to the G3 merge).

- Reconcile typed states `CANDIDATE/TESTED/STABLE/REJECTED/SUPERSEDED/REVOKED`
  in an ADR and implement validated create/list/inspect/test/promote/
  activate/rollback transitions. No public arbitrary status setter.
- Link source commit, parent, reason, actor, signed/trusted manifest,
  snapshot/root identity and immutable evidence. Store activation history.
- Enforce one active stable generation, crash consistency and idempotent
  retry/unknown-outcome semantics at storage and system boundaries.
- Validate forward and backward schema compatibility and durable history
  across daemon restart, machine reboot, interrupted activation and rollback.
- **Exit:** boot A, activate B, reboot into B, rollback A, reboot into A,
  preserving user data and auditable provenance, on an exact-SHA real VM.

## Required CI evidence before 0.0.5 release

- Offline unit/property tests for manifest canonicalization, hashes, trust,
  transition authorization, storage invariants and negative/failure cases.
- Real Arch/KVM guest test of build → boot → activate → reboot → rollback →
  reboot, including interruption/failed-boot recovery and user-data survival.
- Exact-head release push CI, Rust/Docs, security, CodeQL and required policy
  checks; artifacts include manifest, SHA256, serial logs and boot identity.
- Migration/rollback matrix for databases created by newer binaries; never
  silently downgrade schemas or overwrite persistent data.
- Owner review of remaining high-severity security alerts and explicit
  permission before `master` promotion, tag and GitHub Release.

## Deliberately out of scope

- Automatic AI-directed OS authority or self-modification.
- Full Evolution Lab orchestration (0.0.6).
- Distribution-grade installable image and production updater (0.0.9).
- Default PTY enablement without verified systemd cgroup delegation.
- Claiming fully reproducible binaries solely from pinned inputs.

## Immediate next engineering action

Start with #48: author the substrate ADR and an isolated bootable-VM
rollback proof, **before** implementing activation logic. In parallel, #49
can define the package manifest and trusted archive acquisition contract.
Document all negative results and keep warnings visible.

The ordered PR breakdown for #51 + #48 is in
[0.0.5 step 1 — Flowerly boots its own image and survives one real rollback](release-0.0.5-step1-ab-boot-rollback-plan.md).


## 2026-11-07: 16 scoped Generation 0.0.5 contracts (not release acceptance)

Sixteen independently reviewable offline contracts were started on the
verified `release/2` integration branch from SHA
`39260b210753d1f5ac2732f2f2dc13de3f5a8ae0`. Each issue has a separate
draft PR, its own test file and review note. Merge only after exact-head
required checks, architecture review, and explicit maintainer permission;
rebase subsequent branches on the latest release/2 after each merge.

| Order | Issue | Increment |
| --- | --- | --- |
| 1 | [#321](https://github.com/Doldas/flowerly_os/issues/321) | artifact digest |
| 2 | [#322](https://github.com/Doldas/flowerly_os/issues/322) | partition geometry |
| 3 | [#323](https://github.com/Doldas/flowerly_os/issues/323) | booted PARTUUID |
| 4 | [#324](https://github.com/Doldas/flowerly_os/issues/324) | loader entry parsing |
| 5 | [#325](https://github.com/Doldas/flowerly_os/issues/325) | EFI entry identity |
| 6 | [#326](https://github.com/Doldas/flowerly_os/issues/326) | root identity |
| 7 | [#327](https://github.com/Doldas/flowerly_os/issues/327) | fsync witness prototype |
| 8 | [#328](https://github.com/Doldas/flowerly_os/issues/328) | ledger replay |
| 9 | [#329](https://github.com/Doldas/flowerly_os/issues/329) | health verdict |
| 10 | [#330](https://github.com/Doldas/flowerly_os/issues/330) | slot stage preflight |
| 11 | [#331](https://github.com/Doldas/flowerly_os/issues/331) | slot arm admission |
| 12 | [#332](https://github.com/Doldas/flowerly_os/issues/332) | slot status reconciliation |
| 13 | [#333](https://github.com/Doldas/flowerly_os/issues/333) | persistent data marker |
| 14 | [#334](https://github.com/Doldas/flowerly_os/issues/334) | KVM evidence validator |
| 15 | [#335](https://github.com/Doldas/flowerly_os/issues/335) | power-loss matrix |
| 16 | [#336](https://github.com/Doldas/flowerly_os/issues/336) | package-set identity |

These scripts are deliberately **not** wired into firmware, root-identity
systemd units, slot writer, production activation or `flowerlyd`.
A real firmware-booted Flowerly disk, boot-counting/fallback, power-loss
failure injection, user-data partition persistence, signed package snapshot
and required owner decisions remain blocked under #48, #49, #51 and #9.
Completing any of the above offline checks alone does not fulfill S4–S8.
