# 0.0.5 Generation A/B — opt-in OVMF/KVM acceptance harness

Status: **IMPLEMENTED HARNESS / REAL GUEST RUN NOT EXECUTED**. Tracks #48, #51, #9 and #334/#335. Required companion PRs: #353 (stage), #354 (guest witness/health), #355 (pinned GPT PARTUUID) and #356 (EFI arm). Review and merge each on `release/2` with exact-head checks and explicit owner approval; do not automatically merge to master.

## Scope and real verification target

`scripts/generation-ab-kvm-acceptance.py` is a real QEMU/KVM runner, not a fake guest:

- Requires a **clean exact-SHA checkout**, explicit `--approved-sha`, a non-root operator, readable/writable `/dev/kvm`, real QEMU, OVMF CODE+VARS and host GPT/FAT tools; `FLOWERLY_GENERATION_AB_KVM=I_APPROVE_GENERATION_AB_KVM_EXECUTION` is needed for `--execute`. No TCG fallback.
- Requires a Flowerly-built GPT A/B disk with both root slots staged, with source SHA and trusted disk hash verification, plus root A/B baked identities and the boot units in the test profile. Bootloader runs through OVMF; **no QEMU `-kernel` override**.
- Starts A through systemd-boot and requires real kernel/EFI identity, fsynced separate-data witness, service-user IPC Ping, **post-`systemd-bless-boot good`** marker and 32-byte user-data-marker SHA.
- Preserves the live data partition and EFI boot-counting state from A's actual boot, but restores the original pinned root partition bytes to a new disposable image. Arms B with its kernel/initramfs and a **test-profile-only** health failure flag; the prior source disk is never overwritten.
- Reuses the **same writable OVMF_VARS** across B boots, terminates QEMU with SIGKILL for power-loss simulation after each B root witness and health rejection, and demands A's return after the exhausted boot-count, with the same user-data SHA. A successful observation produces `SEQUENCE_OBSERVED_S7_INCOMPLETE` — **never a full 0.0.5 release success**.

No network is configured for the guest. Serial artifacts and the evidence JSON are saved in an exclusive output directory; the current sequence's failure and `NOT_RUN` scenarios remain explicit on error. The harness does not use host process/state mock success to assert a guest has booted.

## Inputs and operator execution

First build **two separately prepared rootfs volumes** with `prepare-generation-ab-rootfs.sh` test profile, one for each root identity, using the same canonical/archived package inputs. Pin both UUIDs via `FLOWERLY_AB_ROOT_A_PARTUUID` and `FLOWERLY_AB_ROOT_B_PARTUUID` in the initial GPT builder, stage B using `generation-slot-stage-image.py`, and verify the stage manifest. Provide the B kernel/initramfs from the same built B rootfs. Use a real matching OVMF CODE/VARS pair.

```bash
export FLOWERLY_GENERATION_AB_KVM=I_APPROVE_GENERATION_AB_KVM_EXECUTION
sha="$(git rev-parse HEAD)"  # Compare with the SHA explicitly approved by owner
python3 scripts/generation-ab-kvm-acceptance.py \
  --approved-sha "$sha" \
  --disk /path/to/staged-pristine-ab.img \
  --manifest /path/to/staged-pristine-ab.manifest.json \
  --kernel-b /path/to/b-built-rootfs/vmlinuz \
  --initramfs-b /path/to/b-built-rootfs/initramfs.img \
  --ovmf-code /usr/share/OVMF/OVMF_CODE_4M.fd \
  --ovmf-vars /usr/share/OVMF/OVMF_VARS_4M.fd \
  --root-a 11111111-1111-1111-1111-111111111111 \
  --root-b 22222222-2222-2222-2222-222222222222 \
  --output /scratch/flowerly-ab-evidence-unique \
  --timeout 180 --tries 3 --execute
```

Without `--execute`, validation is read-only and prints `NOT_RUN`; no real KVM run has been performed in this development session. The exact output paths and UUIDs shown above are **illustrative and must be substituted**. Do not use a host's real installed root device or data partition; this harness only accepts regular disk-image inputs.

## Missing gates and honest status

This tool proves at most one **firmware-booted A → failing B → fallback A** sequence when it actually runs and yields logs. The complete S7 eight-scenario matrix is **not** yet satisfied: interruption immediately after staging, before/after EFI rename on a mounted ESP, tampered image, full disk, clean shutdown, and real production power-loss recovery remain pending. The manifest's original whole-disk hash becomes stale after guest boot because EFI boot counters and data are mutable; the harness treats these as such rather than quietly rewriting a trust baseline. Signed package acquisition, review of rollback substrate, owner D3/D5 and release authorization remain open.

CI runs an offline parser/negative-case test only; hosted PR CI **does not claim KVM**. No branch merge, release tag or master promotion may be based on the existence of the harness alone.


## Artifact audit added after first review (2026-11-07)

The harness now re-reads all expected real guest serial logs after the
A → unhealthy B × tries → A sequence and verifies exactly one root-witness
record and boot ID per log, root A/B PARTUUID, the matching data-marker SHA,
explicit positive post-bless IPC for A, negative health/no blessing for B,
unique filenames, boot-count-sized sequence and exact SHA-256 digests in
`serial-sha256.json`. Any missing, duplicated, contradictory or unexpected
log fails the final acceptance report; all stage/ESP-interruption cases not
actually run remain `NOT_RUN`. A passing audit says only
`INTERNALLY_CONSISTENT_NOT_RELEASE_ACCEPTED`—serial output and its hashes
are not cryptographic proof that QEMU produced them or that the VM was
executed on the approved trusted host. Full S7 KVM power-loss coverage and
owner approval are unchanged requirements.
