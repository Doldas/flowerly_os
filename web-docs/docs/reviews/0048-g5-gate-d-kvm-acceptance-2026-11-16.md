# #9 / ADR-0023 G5 — Gate D real OVMF/KVM acceptance (2026-11-16)

Status: **GATE D SEQUENCE OBSERVED on the exact SHA, 16 of 16 scenarios PASS.**
`release_acceptance` in the harness output stays `false` (it is the S7 slot
audit's flag, not the activation gate). Follows
[the clean-shutdown review](0048-s7-clean-shutdown-2026-11-09.md).

## Result (commit `eb600602c73c1a894ae2e2da996ddb661000ea9c`, run 9)

Evidence: [evidence/0048-g5-gate-d-eb60060](evidence/0048-g5-gate-d-eb60060/)
(`acceptance.json`, every serial log with `serial-sha256.json`, the 13 drill
reports and instruction files; no disk images).
Harness: `scripts/generation-gate-d-kvm-acceptance.py`, guest drill
`testbed/generation-ab/g5-drill.py`, test-only cargo feature
`g5-schema-drill` (migration 18) built into slot B2 only. Three roots A, B and
B2 (schema 18, `min_reader` 18) on one disk; real QEMU/KVM with OVMF and
systemd-boot; the Admin steps run `agenticctl` inside the guest.

| Scenario | Result |
| --- | --- |
| register-tested, activate-a-confirmed | PASS |
| activate-b-confirmed (boot A → activate B → reboot → witness → STABLE) | PASS |
| unhealthy-activation-fell-back (3 tries, forced unhealthy B → FELL_BACK, A boots) | PASS |
| fell-back-needs-acknowledgement | PASS |
| acknowledged-reactivation-confirmed | PASS |
| rollback-b-confirmed (rollback → A) | PASS |
| unhealthy-rollback-fell-back | PASS |
| irreversible-needs-acknowledgement | PASS |
| irreversible-activation-confirmed (B2, schema 18) | PASS |
| downgrade-refused-across-schema | PASS |
| older-root-cannot-read-the-database | PASS |
| restore-drill (backup named by its hash restored by root, then A boots) | PASS |
| user-data-intact (marker hash identical after every step) | PASS |
| history-readable-every-step | PASS |
| exhausted-target-observed | PASS |

## Exhausted target (open question from G4b)

With the target at `flowerly-b+0-3.conf` and the demoted `manual-flowerly-a.conf`
as the only other entry, systemd-boot **kept booting the exhausted target**
(root-b started the loader, 120 s observed, no reboot loop, no menu stall, and
it never booted the demoted root by itself). The incompatible root is therefore
never entered automatically; B stays up and unhealthy until the Admin acts.
The fallback is manual-only, as the ADR says.

## Defects the real run found and fixed

Runs 1–8 each failed for a real reason; none was handled by weakening a check.

1. systemd-boot picks the newest armed entry only when entries carry
   `sort-key`/`version` (5fbb259).
2. The witness ledger was written as group `flowerly` while the multi-user
   drop-in gives the state directory `flowerly-users`; the guest now asks
   systemd for the unit's `Group=` (19ad5ac). The reader's rule is unchanged.
3. Activation exceeded flowerlyd's 7 s module deadline (both slots hashed on
   plan and arm): plan no longer hashes slots; activate answers with the
   PENDING row and arms on a worker thread (943adc5, ee404fd; migration 17,
   `armed_at`).
4. Harness order forced "unhealthy" on a rollback that was refused for another
   reason (dbaa2be); a /proc race right after Popen (736d5c9).
5. The drill report was lost when journald's console forwarding was cut by the
   reboot and by `agetty` flushing ttyS0: the drill writes `/dev/console`
   itself, drains it, and runs before the serial getty (bca8d5a, feb6451).
6. **Witness change:** after an irreversible activation the old root's own
   witness refused its demoted entry `manual-flowerly-a.conf`, so `flowerlyd`
   could not start there and the restore failed. Both the guest writer and
   `flowerly-core::witness::loader_slot` now accept exactly
   `manual-flowerly-a|b[.conf]`; the identity (PARTUUID/slot) check is
   unchanged (8eb8f93).
7. **Harness:** systemd colours "Failed to start …"; the plain-text match never
   saw it, which also made the "blessed boot has no failed supervisor" check
   vacuous. Both checks now strip SGR sequences, which makes the check
   stricter, not weaker (eb60060).

## Known, not changed

- Under `--features g5-schema-drill` four unrelated `flowerly-core` tests fail
  identically on the base (CI only runs the `schema_drill` filter).
- The harness is a manual, owner-approved, exact-SHA run on this host (about
  35 GiB scratch); it is not a CI job. #49 (pinned packages and signatures)
  and block-device/live-ESP writes by `flowerlyd` remain out of scope.
