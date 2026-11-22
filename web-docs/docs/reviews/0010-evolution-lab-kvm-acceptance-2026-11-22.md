# #413 Evolution Lab — real KVM acceptance at `7ce5942` (2026-11-22)

Status: **All three ADR-0024 scenarios pass on a real KVM host at
`release/2`'s exact SHA. The evidence is archived; the owner reviews it.**

## What was done

One `vm-test` session ran at
`7ce5942eb29826e5e8363e4168d4ad42e65198a8`. That is `release/2` after #416–#421
were merged (E1–E4, E6 and the archive tool), and the head of promotion #424.
The session had three scenarios, all in the reproducibility lane, from one
candidate built at that commit:

1. A clean, detached worktree at that commit built the release binaries and
   the candidate, then ran `good`
   (`FLOWERLY_LAB_OUT=target/lab/e5-good bash scripts/vm-test.sh --lane reproducibility --scenario good`).
2. `bad` and `rollback` reused that candidate through
   `FLOWERLY_LAB_CANDIDATE=target/lab/e5-good/candidate`. They were not rebuilt.
3. `scripts/archive-lab-evidence.py archive` from the same commit checked the
   three runs against the candidate and the SHA, then archived them.
   `archive-lab-evidence.py check --evolutiond target/release/evolutiond`
   re-read the archive and exited 0.

The lab inputs were the cached pinned rootfs and initramfs from the 0.0.5 Gate B/D
build (frozen package set `8689cd41…4d62`, [Gate B review](0049-gate-b-frozen-package-set-2026-11-16.md)).
No network was used and nothing was downloaded.

Evidence: [evidence/0010-evolution-lab-7ce5942](evidence/0010-evolution-lab-7ce5942/).
`evidence.json` (`flowerly-lab-evidence/v1`) has SHA-256
`91a16faf0d43e1f6166788fb7645be74a96c2f1184ac3f89c56e95bafcb2d5c6`. Disks,
firmware variables and databases are not archived.

## Host

| Item | Value |
| --- | --- |
| host | Ubuntu 26.04.1 LTS, kernel 7.0.0-43-generic, AMD Ryzen 5 7600 (12 threads), 29 GiB |
| accelerator | `kvm` (recorded in every plan and result) |
| QEMU | 10.2.1 (Debian 1:10.2.1+ds-1ubuntu3.3), `/usr/bin/qemu-system-x86_64` SHA-256 `fa552a45…44cb` |
| OVMF | `ovmf` 2025.11-3ubuntu7.3, `OVMF_CODE_4M.fd` `874f15a7…8aa7`, `OVMF_VARS_4M.fd` `5d2ac383…5d1e` |
| user | non-root; the candidate build ran inside `podman unshare` |

## Candidate

`candidate.json`: the reproducibility lane, built at `7ce5942`. Slot A
(`lab-a`) is active and slot B (`lab-b`) is staged. Both slots carry the probe and the same
Flowerly binaries.

| Item | SHA-256 |
| --- | --- |
| input manifest | `8689cd419c928daa361be330b1794e5be06b301773a7bb94ba5febc97a424d62` |
| kernel (`7.2.14-arch1-1`) | `b1e9eaf5700a53a31ed923edecd72b319a29da74be32c0885bff112d37ce325b` |
| initramfs | `1fbfd657aaa3abc4857a9b6bfea8bbd9006fc3698665fca48393e9b26d235465` |
| root-a identity | `77273c80a1e1fe55e5f7da0a5683f3fa13d469c0e7cfb081e5d78542c0819dfe` |
| root-b identity | `57e232bf32bb9603e0bdb376f2d40298b4f4ef9ebe862d5195864f808ea4b882` |

## Results

| Scenario | Generation | Experiment | Boots | Outcome | Result digest |
| --- | --- | --- | --- | --- | --- |
| good | `gen-92bc628809f64de68f5bf6611fbf76b1` | `exp-b902a0548afe48889d5fd53eafe38570` | candidate (root-b): all checks pass | passed, recommend test → **TESTED** | `8a1a1a64…b40d` |
| bad | `gen-43e660a3917b446eb165f9595be37314` | `exp-50ef4c8bc8f14d719bdbdf47388eba6e` | candidate (root-b): health gate `failed` (`flowerly-generation-health.service`) | failed, recommend reject → **REJECTED** | `b4fdf7a7…3a69` |
| rollback | `gen-3eb186b7546f45e4941a5a8155f12b79` | `exp-4bebb4d9de474f9484c74a0d690db7aa` | candidate-1 and candidate-2 (root-b): health `failed`; fallback (root-a): booted, healthy | failed, recommend reject → **REJECTED**; healthy fallback to root-a | `c0da2852…30c8` |

The full result digests are in `evidence.json`. Every initial boot, an
unrecorded boot of slot A that the guest requires before B is armed, passed.
Each Generation's state was recorded by the scenario's private `flowerlyd`,
whose log is archived beside the result.

## Findings

- The roadmap exit criterion is met for the reproducibility lane:
  `just vm-test` builds a candidate, boots it, verifies a service, exits and
  records success or failure. It did this on a real KVM host, from the exact
  source SHA, with the three ADR-0024 scenarios archived.
- The `bad` and `rollback` failures are the injected health failure the
  scenarios require, not lab defects. The other checks (binaries, identity,
  integration) passed on every boot.
- The runner's QEMU TCG job for promotion #424 ran on the same host during
  the session. No timeout or retry was needed. Every boot took 8.2–8.8 s
  from start to probe report.
- Not covered here: the upstream lane (needs the owner's network approval)
  and the opt-in `lab-vm-test` CI job, which GitHub can dispatch only once
  the workflow is on the default branch. Neither is reproducibility evidence.
