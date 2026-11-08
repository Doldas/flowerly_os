# #48/#51 evidence — first real OVMF/KVM A → unhealthy B → fallback A run (2026-11-08)

Status: **SEQUENCE OBSERVED, S7 INCOMPLETE.** `release_acceptance` is `false`.
This is the first execution of `scripts/generation-ab-kvm-acceptance.py`
(owner approval 2026-11-08: "kör S7"). It does **not** make Generation
rollback a `flowerlyd` feature and does not accept ADR-0019.

## What ran

- Code: commit `b82704981859fddc0cafdff6fd315381dee0a446` on
  `fix/48-mount-esp-for-bless-boot` (release/2 `7c17c3d` plus the seven fixes
  below). Clean worktree, non-root operator, QEMU 10.2.1, `/dev/kvm` held open
  by every guest process, OVMF `OVMF_CODE_4M.fd`/`OVMF_VARS_4M.fd`, no network,
  no `-kernel` (firmware → systemd-boot → entry).
- Images: rootfs from `build-arch-rootfs.sh` (mkosi in a rootless privileged
  Arch container), Flowerly release binaries of the same commit, two slot
  volumes prepared with `prepare-generation-ab-rootfs.sh --test-profile`
  (distinct PARTUUIDs and build ids), disk from `build-flowerly-ab-disk.sh`,
  slot B staged with `generation-slot-stage-image.py`. Package inputs are the
  rolling Arch set of that day and **not pinned** (#49); the
  `input-manifest-sha256` is the digest of the installed `name-version` list,
  not a frozen manifest.
- Evidence bundle: [evidence/0048-s7-b827049](evidence/0048-s7-b827049/)
  (acceptance JSON, serial logs, serial hashes). Disk images are not archived.

| Artifact | SHA-256 |
| --- | --- |
| kernel `vmlinuz` | `b1e9eaf5700a53a31ed923edecd72b319a29da74be32c0885bff112d37ce325b` |
| initramfs | `c1a1406d5de2993e86bdb42915f79ffd363f89cff5bf3e0f4e53fc9b7a06262d` |
| root-a volume | `3e9192caf8e1439bad609a4ef4dfc8cda46de43056b433174ba25781698acf84` |
| root-b volume | `b613064ce8969aede25f62c5022d5b45e8ce8206ff0ac867029628b7117ef0b6` |
| pristine staged disk | `44f29df7a611244afa0f281ce5d3cb10e2ba272ae16f9f20fd4f279e8bfbba97` |

## Observed sequence

| Boot | Entry | Observed |
| --- | --- | --- |
| `boot-a` | `flowerly-a+3` | root-a verified, IPC ping OK, `systemd-bless-boot good` ran |
| `unhealthy-b-1..3` | `flowerly-b+3` with `flowerly.test.fail_health=1` | root-b verified, health rejected, never blessed (SIGKILL after each) |
| `auto-fallback-a` | `flowerly-a` (loader picked it) | root-a, IPC OK, same 32-byte user-data marker SHA-256 |

Passed scenarios: `boot-before-bless`, `data-survives`,
`unhealthy-b-fallback`. Both root partitions were byte-identical to the pinned
manifest after the first and after the fallback boot. The independent
`generation-ab-evidence-audit.py` returned
`INTERNALLY_CONSISTENT_NOT_RELEASE_ACCEPTED` (5 boots, one boot id per log).

## Defects the run found (all fixed in this branch, one commit each)

1. ESP not mounted in the guest: `systemd-bless-boot` "Couldn't find $BOOT".
   Mount `PARTLABEL=esp` at `/efi`.
2. Root mounted `rw` dirtied the pinned image. Entries now boot `ro`.
3. A plain read-only root broke logind and other services. Added
   `systemd.volatile=state` (tmpfs `/var`).
4. With a read-only root `systemd-sysusers` cannot create `flowerly`. Accounts
   are baked in at image preparation; the oneshot witness is
   `RemainAfterExit` so `Requires=` does not run it twice.
5. initrd `fsck` rewrote the ext4 superblock of the "immutable" slot
   (`wtime`, `kbytes_written`, checksum). Added `fsck.mode=skip`.
6. `loader.conf` `default flowerly-b*` kept selecting B after its tries ran
   out. Now `default flowerly-*`: boot counting sorts the exhausted entry last
   and the loader falls back to A with no Flowerly code in the decision.
7. `systemd-bless-boot good` fails on an entry with no boot counter (A after
   fallback). `generation-bless.sh` blesses only when `LoaderBootCountPath`
   exists.

The harness also copies the 7 GB disk into `tempfile` space; on this host
`/tmp` is a 15 GB tmpfs, so run with `TMPDIR` on a disk-backed path.

## Not shown (S7 still open)

- A *healthy* B: stage + arm B, boot, bless, stay on B, then roll back to A.
- Power loss after stage, around the loader-entry rename, and during B's boot
  before bless (`stage-interrupted`, `arm-before-rename`, `arm-after-rename`
  remain `NOT_RUN`); tampered B image and full-disk refusal in the guest flow
  (`tampered-b-refused`, `stage-complete`).
- Measurements (bytes per slot, stage time, clean shutdown time).
- Pinned/signed inputs (#49), schema-downgrade check across slots, `flowerlyd`
  Generation activation (Gate D), owner D5 on ADR-0019.
- The evidence is serial-log based and self-consistent; it is not
  cryptographic proof of the host run.
