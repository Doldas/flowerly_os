# #48 evidence — S7 matrix on real OVMF/KVM: healthy B, rollback, power-loss cases (2026-11-08)

Status: **ALL 12 CATALOGUED SCENARIOS OBSERVED, S7 STILL NOT ACCEPTED.**
`release_acceptance` stays `false`; ADR-0019 stays Proposed (owner D5).
Follows [the first run](0048-s7-first-real-kvm-run-2026-11-08.md). Owner
approval: 2026-11-08 ("kör scenariot med frisk B + rollback och
strömavbrottsfallen").

## What ran

- Commit `90492ed4ec87e25adbf25bb68c8508e9d0c4a22e` (release/2 `361c3b5` plus
  the harness extension), clean worktree, non-root, QEMU 10.2.1, every guest
  with an open `/dev/kvm` descriptor, OVMF, no network, no `-kernel`.
- Images built from that commit by the same pipeline as the first run
  (mkosi rootfs, two slot volumes, GPT disk, slot B staged). Package inputs are
  the rolling Arch set of the day, **not pinned** (#49).
- Evidence: [evidence/0048-s7-90492ed](evidence/0048-s7-90492ed/) (JSON, 15
  serial logs, hashes). The independent audit
  (`generation-ab-evidence-audit.py`) returned
  `INTERNALLY_CONSISTENT_NOT_RELEASE_ACCEPTED`; it now refuses any PASS that its
  own observed evidence does not support.

| Artifact | SHA-256 |
| --- | --- |
| kernel `vmlinuz` | `b1e9eaf5700a53a31ed923edecd72b319a29da74be32c0885bff112d37ce325b` |
| initramfs | `bbf585c148630843f78ab1717ff132aa9147c7585a996905f4beccded24715a8` |
| root-a volume | `9ed36e3cb9308af5bc71507547b8f8f308d2b50386fd03ceaec0196c2fa2489f` |
| root-b volume | `76b3ec0f28c17c3fc695bb33467d14721961c737afa3de89dba0bd71bae1b2dd` |
| unstaged GPT disk | `c0b33805090b5298488f4006b3ddd53110878aac9f3735c16f0a858f3b65ca66` |

## Results (15 real boots)

| Scenario | Observed |
| --- | --- |
| `boot-before-bless`, `unhealthy-b-fallback`, `data-survives` | A blessed; B with `fail_health` rejected x3; loader returned to A; user-data marker unchanged (as in run 1) |
| `healthy-b-activation` | B armed healthy: blessed, then a **second boot stayed on B** (no counter left, loader still picked B) |
| `rollback-to-a` | B's entry removed and `default flowerly-a*` set (fixture for the future slot tool); next boot blessed A, same marker |
| `arm-before-rename` | payload on ESP + entry only under `flowerly-b+3.conf.tmp`: loader booted A |
| `arm-after-rename` | entry renamed in but default not yet updated: loader booted A |
| `damaged-entry-fallback` | B entry cut off mid-PARTUUID: the loader burned all 3 tries (kernel started, initrd waited for a non-existent root, no Flowerly marker), then booted A with the same marker |
| `stage-interrupted` | power loss after fsynced chunk 300 of the slot write: ESP, root-a and data byte-identical, manifest untouched, re-verification refuses the half-written disk, and the real guest still boots and blesses A without touching either root slot |
| `stage-complete` | full 3 GiB slot stage verified (31.2 s); an oversized candidate is refused ("does not fit exact root slot") with the disk unchanged |
| `tampered-b-refused` | one flipped byte in root-b: layout verifier, `arm_image` and stage digest check all refuse, ESP unchanged, no outputs created |
| `measure-stage-and-boot` | slot sizes (root 3 GiB each, ESP 256 MiB, data 512 MiB), stage time 31.2 s, boot-to-witness+health 8.0-11.5 s |

Both root partitions were byte-identical to the pinned manifest after every
boot, using a read-only root, `systemd.volatile=state` and `fsck.mode=skip`
(run 1 defects).

## Defects this run found (fixed in this branch)

1. A torn B entry cut `console=ttyS0` off the options line, so the kernel
   started silently (serial showed nothing). `console=` and `loglevel=6` now
   come first in the entry. The burned try was visible all along (`b+2-1`).
2. Arch's default console loglevel hides `Kernel command line:`; without
   `loglevel=6` early the audit could not prove which slot the loader started.

## Honest limits (what this is not)

- The arm and stage "power loss" cases act on **disk images**: the
  interrupted ESP states are constructed by the arm fixture and the stage cut
  is fault injection in the stage tool. No live writer to a mounted ESP or block
  device was killed mid-write, because the production slot tool (S6) does not
  exist yet. This is the only slot-level evidence possible today.
- `boot-before-bless` kills B after its health rejection, not at an
  arbitrary instruction; clean shutdown time is **NOT_MEASURED** (needs a QMP
  channel the approved QEMU argv does not have). Both are recorded in
  `s7_remaining`.
- Rollback is a fixture operation (entry removal + default change), not a
  `flowerlyd` or tool feature. Gate D is untouched.
- Inputs are unpinned (#49); evidence is serial-log based and self-consistent,
  not cryptographic proof of the host run.
