# 0.0.5 step 1 — Flowerly boots its own image and survives one real rollback

Status: **IN PROGRESS** — S1, S2 and S3 done 2026-11-01. Written against
`release/2` `95fc9f5`; S1+S2 merged as #278 (`3804bbb`). Re-verify the branch
head, issue state and CI before starting any item; this document is a
snapshot.

Parent plan: [0.0.5 Generations plan](release-0.0.5-generations-plan.md).
Issues: [#51](https://github.com/Doldas/flowerly_os/issues/51) (bootable
fixture), [#48](https://github.com/Doldas/flowerly_os/issues/48) (rollback
substrate), [#9](https://github.com/Doldas/flowerly_os/issues/9) (tracking).
Design input: [ADR-0019 (Proposed)](adr/0019-generation-rootfs-rollback-proposal.md).

## Why this is the next step

Gate D (durable Generation activation in `flowerlyd`) cannot be built
truthfully until there is something real to activate. Today:

- `distro-builder manifest check|verify-cache` exists (#49, partial), so
  input identity has a format, but no frozen package set.
- `scripts/build-arch-rootfs.sh`, `build-arch-root-volume.sh`,
  `build-arch-initrd.sh`, `prepare-arch-boot.sh` and
  `probe-arch-qemu-boot.py` exist (#51, PRs #163–#175), but **no successful
  boot of a Flowerly-built root volume with an in-guest IPC health witness
  has been recorded**. The ext4 volume builder has an open permission
  blocker on `dbus-daemon-launch-helper`.
- Generation rollback is metadata-only: `activate_generation` flips SQLite
  status rows; nothing selects a different root on the next boot.

Step 1 therefore combines Gate C (#51) and the Gate A prototype (#48) into
one deliverable:

> A Flowerly-built two-slot disk boots slot A in KVM, stages and boots
> slot B, automatically returns to A when B fails its health gate, survives
> injected power loss at every boundary, and keeps a separate persistent data
> partition intact throughout — with exact-SHA evidence.

This step does **not** wire activation into `flowerlyd` and does **not**
claim a Generation rollback feature. It produces the substrate and the
evidence that Gate D will build on.

## Owner decisions required

| ID | Decision | Needed before |
| --- | --- | --- |
| D1 | **Answered 2026-11-01:** owner asked to proceed on A/B slots as the prototype substrate (recorded in ADR-0019; the substrate itself is accepted only after S7) | S3 |
| D2 | **Answered 2026-11-01:** owner allowed the downloads the build needs (Arch container, packages); still unpinned until #49 | S1 |
| D3 | Approve each KVM execution against an exact SHA (same model as #263) | S2, S7 |
| D4 | Merge authorization per PR into `release/2` | each PR |
| D5 | Whether the ADR may move to **Accepted** after the S7 evidence review | S8 |

## Target disk layout (prototype, subject to D1)

```text
GPT disk (raw, sparse)
├─ ESP        vfat   systemd-boot, one loader entry per slot, boot counting
├─ root-a     ext4   immutable rootfs image A (+ kernel/initramfs on ESP)
├─ root-b     ext4   immutable rootfs image B, or empty
└─ data       ext4   /var/lib/flowerly persistent state + witness ledger
```

- Slot selection uses **systemd-boot boot counting** (`entry+3.conf`
  tries) and `systemd-bless-boot.service`, gated by a Flowerly health unit.
  A failed or unhealthy boot exhausts its tries and the loader falls back to
  the last good entry with no Flowerly code in the decision path.
- Root identity = partition UUID **and** the SHA-256 of the slot image,
  plus a `generation-identity` file baked into the image at build time
  (manifest digest, Flowerly commit, build id).
- The data partition is never part of a slot and is never written by the
  slot-staging tool. SQLite schema compatibility already fails closed on a
  newer database (#247); that guard is part of the A → B → A test.
- Out of scope for this step: dm-verity, Secure Boot, UKIs, TPM. Note them
  as follow-ups in the ADR, do not build them.

## Workstreams (one focused PR each)

Branch names must start with `feature/`, `fix/` or `docs/`. Every PR runs
`bash scripts/preflight-docs.sh` in a clean worktree before push, and adds
offline tests (fake tools, no KVM) next to any new script, following
`scripts/test-*.py`.

### S1 — Fix the ext4 root-volume ownership blocker (#51)

**Status: DONE in #278 (`a6a7833`).**

- Reproduce the `dbus-daemon-launch-helper` permission failure in
  `build-arch-root-volume.sh` and record the exact error.
- Build the volume without needing to read root-only files as an
  unprivileged user: `mkfs.ext4 -d` inside the same user namespace mkosi
  used, or run the step in the disposable root guest. Do not chmod the
  rootfs to make it readable.
- Offline test: a fixture tree with a `0o4750 root:dbus` file must come out
  of the volume with the same owner, group and mode (check with `debugfs`).
- **Exit:** a real local build produces a volume; ownership/mode audit
  passes for all setuid/setgid files. (Met with the real mkosi tree, built
  in a rootless privileged Arch container with no host install.)

### S2 — First real boot of the Flowerly root volume (#51)

**Status: DONE in #278 (`3155fcf`); evidence in [reviews/0051-flowerly-volume-kvm-boot-2026-11-01](reviews/0051-flowerly-volume-kvm-boot-2026-11-01.md).**

- Run `probe-arch-qemu-boot.py` (direct kernel boot) against the S1 volume
  under KVM, with the existing exact IPC health witness (#173) and boot
  evidence manifest (#175).
- Reuse the gating and cleanup patterns proven in
  `remote-provider-arch-kvm-acceptance.sh`: `--plan/--validate/--dry-run`
  side-effect free, `--execute` behind an env gate + exact SHA + clean
  tree, global-state cleanup trap, no secrets in evidence.
- **Exit:** archived serial log, QEMU version/cmdline, kernel/initramfs/
  volume hashes, in-guest `flowerlyd` health response. Close the matching
  #51 acceptance boxes; #51 itself closes only after S4 (built disk, not
  direct kernel boot).

### S3 — Substrate ADR decision (#48, docs only)

**Status: DONE in the docs PR that added this file; ADR-0019 stays Proposed.**

- Update ADR-0019 with D1's answer: layout above, boot counting, root
  identity, two-phase staging protocol, recovery modes, GC rule (never
  delete active, pending or last healthy slot) and the measured-evidence
  list from S7.
- Record why A/B was chosen over btrfs snapshots for the prototype
  (falsifiable slot identity) and what would trigger revisiting it
  (2× root storage, slot-count limit).
- Status stays **Proposed** until S8.

### S4 — Partitioned two-slot disk builder (#51, #48)

- New `scripts/build-flowerly-ab-disk.sh`: `systemd-repart` (or `sfdisk`
  if repart is unavailable on the builder) creates the GPT layout; copies
  the S1 volume into `root-a`; installs systemd-boot, kernel and initramfs
  on the ESP; writes `flowerly-a+3.conf`; formats an empty `data`.
- Writes a disk manifest: partition UUIDs, per-slot image SHA-256,
  ESP file hashes, builder commit. Fails closed on any hash mismatch.
- Offline tests with fake `systemd-repart`/`bootctl`/`mkfs` record argv and
  check the manifest; one real local build is attached to the PR.
- **Exit:** the disk boots in KVM through firmware (OVMF) → systemd-boot →
  slot A, no `-kernel` flag. This closes #51.

### S5 — Root witness and health gate (#48)

- `flowerly-root-witness.service` (early, `Before=flowerlyd.service`):
  reads `/proc/cmdline`, the root partition UUID, the baked
  `generation-identity` and `LoaderEntrySelected` from efivars, and
  appends one JSON line to `/var/lib/flowerly/boot-witness.jsonl` on the
  data partition with `fsync`. It never decides anything.
- `flowerly-boot-health.service`: waits for the `flowerlyd` IPC health
  witness with a bounded timeout; only on success does
  `systemd-bless-boot` mark the entry good. On failure it logs and
  exits non-zero; boot counting does the rest.
- A test-only kernel cmdline switch (`flowerly.test.fail_health=1`) makes
  the health check fail deterministically. It must be refused unless the
  image was built with the test profile, and the test proves that.
- Units are staged by `install-flowerly-rootfs.sh` and covered by its
  offline test.

### S6 — Slot staging tool, two-phase (#48)

- Prototype CLI `distro-builder slot stage|arm|status` (Rust, in the
  existing crate; no daemon, no IPC yet):
  - `stage <image> --slot b`: refuse the active slot, check free space,
    write, `fsync`, re-read and compare SHA-256 against the manifest.
    No ESP change.
  - `arm --slot b`: write `flowerly-b+3.conf` atomically (temp + rename +
    `fsync` of file and directory) and set the one-shot/default entry with
    `bootctl`. Record a *pending* line in the ledger. No "success" here.
  - `status`: combine the ledger with the current witness; report
    `PENDING`, `ACTIVE` or `FELL_BACK`. Never infer success from the
    ledger alone.
- Caller-supplied paths are not authority: the tool resolves the slot by
  partition type/label from the disk manifest.
- Unit tests for every refusal; failure-injection tests that abort after
  each write step and check the next `status` call.

### S7 — KVM acceptance harness and real run (#48)

New `scripts/generation-ab-kvm-acceptance.sh` + `testbed/generation-ab-kvm/`
check catalog and evidence schema, structured like the remote-provider
harness. Scenarios (ADR-0019 prototype acceptance 1–8):

1. Build images A and B from the same pinned inputs, differing only in a
   marker and build id; hashes differ and are recorded.
2. Boot A; witness reports A. Write a user-data marker to `data`.
3. Stage + arm B; reboot; witness reports B; marker still present.
4. Arm B again with `fail_health`; after the tries run out the witness
   reports A and `status` says `FELL_BACK`. Marker intact.
5. Power loss: kill QEMU after `stage`, after `arm` (before and after the
   rename), and during B's boot before bless. Each next boot lands on a
   verified root and `status` is truthful, with no manual repair.
6. Negative: tampered B image, full disk at stage, damaged loader entry,
   interrupted stage. No ESP change and no false success in every case.
7. Measure: disk bytes per slot, stage time, boot-to-bless time, clean
   shutdown time.
8. Evidence bundle: exact SHA, QEMU/OVMF versions, serial logs, witness
   ledger, disk manifest, hashes; validated by the schema, no credentials.

A scenario that cannot run is `NOT_RUN` or `INCONCLUSIVE`, never `PASS`.
The first real run needs D3 and may surface harness defects; fix them in
separate commits and re-run against the new exact SHA.

### S8 — Evidence review and issue updates (docs)

- Owner reviews the S7 bundle. On approval (D5) ADR-0019 becomes Accepted
  with links to the run; #48 deliverables ticked with evidence; #9 gets the
  substrate reference; this plan and the parent plan are updated.
- Record negative results and open defects as issues, not as caveats in
  chat.

## Ordering and parallelism

```text
S1 ──► S2 ──► S4 ──► S5 ──► S6 ──► S7 ──► S8
        S3 (docs, after D1) ──────┘
```

S3 can run in parallel with S1/S2 once D1 is answered. Two parallel
tracks that do not block step 1:

- **#49 trust policy:** decide the keyring and expiry-on-replay rule and
  pick the Arch archive snapshot date that S1 builds from. Until a real
  frozen package set exists, step 1 evidence states "same pinned snapshot
  for A and B", not "reproducible inputs".
- **Gate D ADR draft (#9, docs only):** the typed transition table, and the
  two findings already recorded: `generation.activate` has no transition
  check or activation history, and `generation.create` takes free-form
  `source_commit`/`system_manifest`. `set_generation_status` must lose its
  public reach (today its only caller is the in-binary `self_test` in
  `src/flowerlyd/src/generation/mod.rs`, not an IPC path). Implementation
  waits for S8.

## Exit criteria for step 1

- [ ] #51 closed with a firmware-booted Flowerly disk and in-guest IPC
      health evidence.
- [ ] S7 run on one exact `release/2` SHA: all eight scenario groups PASS,
      evidence bundle archived and schema-valid.
- [ ] User-data marker survives A → B → A and every power-loss case.
- [ ] ADR-0019 Accepted by the owner (or reworked, with the reason written
      down).
- [ ] Offline tests for every new script and the slot tool run in the
      required `docs`/`rust` jobs and pass on the root push runner.
- [ ] No text anywhere claims Generation rollback exists in `flowerlyd`.

## Risks and known unknowns

- **Firmware/boot counting in QEMU:** OVMF variable persistence across
  QEMU restarts needs a writable `OVMF_VARS` copy per run; without it boot
  counting silently does nothing. S4 must prove the counter decrements.
- **Host resources:** the shared self-hosted host also runs other
  services and has had an unclean reset (see the CI runner notes). Size
  the guest and disk explicitly and use the existing resource monitor and
  failure classifier.
- **KVM access:** the user `gamitron` is in `kvm`, but long-lived processes
  may need `sg kvm` or a new login.
- **Space:** two full root slots double root storage; measure in S7 before
  anyone treats A/B as the production answer.
- **Scope creep:** no daemon integration, no updater, no installer, no AI
  authority over activation in this step.
