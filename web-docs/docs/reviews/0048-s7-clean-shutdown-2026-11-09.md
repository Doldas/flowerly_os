# #48 S7 — clean shutdown measured over QMP, S7 matrix complete (2026-11-09)

Status: **ALL 13 S7 SCENARIOS OBSERVED; NOT RELEASE-ACCEPTED.**
`release_acceptance` stays `false`. This closes the last measurement the
step-1 plan and ADR-0019 criterion 7 still listed as `NOT_MEASURED`. Follows
[the slot tool review](0048-s6-slot-tool-2026-11-09.md).

## What changed in the harness

One boot per run (`clean-shutdown-a`) is started with a QMP unix socket
(`-qmp unix:…,server=on,wait=off`; its argv hash is recorded like any other
boot, every other boot still has no control channel). Once the guest is
blessed, `system_powerdown` (the ACPI power button) is sent and the guest's own
orderly shutdown is timed until QEMU exits with status 0; a guest that does not
power off within 120 s, or QEMU exiting non-zero, fails the run. The data
partition is then copied and inspected with `dumpe2fs -h`: it must be
`clean` without `needs_recovery`. The SIGKILLed first boot is inspected the same
way for contrast. The audit requires the boot, a plausible time and the clean
state before it accepts the new `clean-shutdown` scenario.

## Result (commit `4e44081d466a2c080a564526ffffde84c2e406e3`, real OVMF/KVM)

17 boots, 13 scenarios PASS, audit `INTERNALLY_CONSISTENT_NOT_RELEASE_ACCEPTED`.
Evidence: [evidence/0048-s7-clean-shutdown-4e44081](evidence/0048-s7-clean-shutdown-4e44081/).

| Measurement | Value |
| --- | --- |
| clean shutdown (power button → QEMU exit) | **0.77 s** |
| data fs after clean shutdown | `clean`, no `needs_recovery` |
| data fs after the SIGKILLed first boot (contrast) | state `clean`, **`needs_recovery`: true** (journal replay pending) |
| 3 GiB slot stage | 14.5 s |
| boot → witness + health | 8-11 s |
| disk | ESP 256 MiB, root slots 3 GiB each, data 512 MiB (root storage doubles) |

The serial log shows the orderly unit stop sequence and `reboot: Power down`.
The 0.77 s is small because the test guest is tiny (read-only root, tmpfs
`/var`, a handful of services); it is a property of this fixture, not a promise
for a full system.

## Nightly VM budget (#51 acceptance, this host only)

Measured on this self-hosted machine (12 cores, KVM): mkosi rootfs + initrd +
two slot volumes + GPT disk build ≈ 7 min (the rootfs is 1.6 GiB, the images
3 GiB each, ≈ 7 GiB of disk for the matrix plus ≈ 25 GiB of transient copies);
the full 17-boot KVM matrix with host scenarios ≈ 9 min. Roughly 17 minutes and
≈ 35 GiB of scratch space per exact-SHA run. Not yet a CI job: it needs the
opt-in approval gate, KVM and a runner with that disk, so it stays a manual,
owner-approved run (#285 tracks stage timings for the hosted Arch VM job).

## Limits

- The harness still only runs on a disposable image on this host; there is no
  CI job, no block-device/ESP broker (S6 limits stand), no candidate signature
  check (#49) and no `flowerlyd` integration (Gate D).
- Serial-log evidence is self-consistent, not cryptographic proof of the run.
