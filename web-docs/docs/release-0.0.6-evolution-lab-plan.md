# Flowerly OS 0.0.6 — Evolution Lab implementation and acceptance plan

Status: **Started 2026-11-21.** Design: [ADR-0024](adr/0024-evolution-lab-experiment-pipeline.md)
(accepted 2026-11-22). Tracking: [#10](https://github.com/Doldas/flowerly_os/issues/10)
(roadmap) and [#3](https://github.com/Doldas/flowerly_os/issues/3)
(vertical slice, `just vm-test`).

## Progress

E1–E4, E6 and E5's archive tool are merged to `release/2` (#416–#421).
E5's exact-SHA KVM acceptance passed at `7ce5942`, `release/2`'s head after
those merges: `good` TESTED, `bad` REJECTED, `rollback` REJECTED with a
healthy fallback ([review](reviews/0010-evolution-lab-kvm-acceptance-2026-11-22.md)). ADR-0024 was accepted by the owner
on 2026-11-22.

Open:

- E5: the owner's review of the evidence.
- The upstream lane's first networked run.
- The CI job's first run on the runner. GitHub allows it only once the
  workflow is on the default branch.

## Baseline and branch discipline

- v0.0.5 is published (tag `v0.0.5`; promotion #388 as `cd29a9d` on `master`).
- At kickoff `release/2` is `57e078ec7973a4d2cae136cd72cc25f42d561773`
  (0.0.5 close-out #407); the standing promotion PR is #408.
- Work lands in small `feature/*`, `fix/*` or `docs/*` branches targeting
  `release/2`, as draft PRs with exact-head CI. The owner merges; nothing
  is promoted to `master`, tagged or published without the owner.
- Each PR updates this plan for the workstream it changes.

## Exit criteria (roadmap)

`just vm-test` builds a candidate, boots it, verifies a service, exits, and
records success or failure — on a real KVM host, from the exact source SHA,
with the scenarios of ADR-0024 (good candidate, bad candidate, rollback after
a bad candidate) and their evidence archived.

## Ordered workstreams

### E1 — Complete Experiment records (`flowerly-core`, `flowerlyd`, `agenticctl`)

Issue: [#409](https://github.com/Doldas/flowerly_os/issues/409). Merged PR:
[#416](https://github.com/Doldas/flowerly_os/pull/416).

- `generation.evidence` accepts the optional `experiment` object of ADR-0024
  D5 and writes `hypothesis`, `baseline`, `test_plan`, `metrics`; each
  artifact becomes an `artifacts` row (`sha256:<hex>`) with a `produced`
  edge. Idempotent on `(candidate, evidence_sha256)` as today.
- `generation.test` / `generation.reject` set `decision`; `reject` may name
  the Experiment (it must be about that Generation).
- `experiment.inspect` and `experiment.list` (Admin, read-only) with
  `agenticctl experiment inspect|list`.
- Bounds and validation for every field; negative tests; no migration.
- **Exit:** unit and black-box CLI tests; docs in
  `generation-ipc-v1-arguments.md`.

### E2 — `evolutiond lab` (Rust: `vm`, `benchmarks`, `promotion`)

Issue: [#410](https://github.com/Doldas/flowerly_os/issues/410). Merged PR:
[#417](https://github.com/Doldas/flowerly_os/pull/417).

- Plan (`flowerly-lab-plan/v1`) and result (`flowerly-lab-result/v1`)
  documents with canonical JSON and SHA-256.
- `vm`: QEMU argv for OVMF/KVM (TCG only on explicit request), private
  copies of the firmware variables, serial capture to a file, a hard deadline,
  process-group kill on timeout, one QEMU process per boot.
- Probe report parser (numbered base64 parts + digest line), fail closed on
  gaps, duplicates, oversize or digest mismatch.
- `benchmarks`: host-measured boot-to-report and shutdown times, guest boot
  phases, image sizes — recorded as metrics with units.
- `promotion`: the fixed policy of D3; never recommends activation.
- `lab run`, `lab verify` (rehash artifacts, recompute the verdict),
  `lab record` (through flowerlyd: evidence, then test|reject). The
  candidate Generation and its identity already exist; E4 creates them.
- **Exit:** unit tests for argv, parser, policy and result binding; a
  fake-QEMU test of the run loop (timeout, crash, missing report).

### E3 — Guest probe (test profile)

Issue: [#411](https://github.com/Doldas/flowerly_os/issues/411). Merged PR:
[#418](https://github.com/Doldas/flowerly_os/pull/418).

- `testbed/evolution-world/lab-probe.py`, its unit (ordered after
  `flowerlyd` and the health gate, before the serial getty) and a sysusers
  entry for the lab Admin account, installed by the test-profile script only.
- Facts: baked identity (its hash and `build_id`), booted root PARTUUID,
  kernel release and `vmlinuz` hash, installed packages, Flowerly binary
  hashes, boot phases, failed units. Observations: health (`flowerlyd`
  active, status answers), integration (an Admin write/read round trip
  through `agenticctl`, `evolutiond --self-test`). The guest's flowerlyd has
  its own database, so the guest cannot know the host's Generation id; the
  identity's `build_id` is what its Generation's root identity records.
- Writes the report to the console itself and powers the guest off.
- **Exit:** offline Python tests with fake commands in the preflight.

### E4 — Candidate build and `just vm-test`

Issue: [#412](https://github.com/Doldas/flowerly_os/issues/412). Merged PR:
[#419](https://github.com/Doldas/flowerly_os/pull/419).

- `scripts/build-lab-candidate.sh`: slot images from the pinned rootfs, the
  probe installed, A/B disk, optional bad candidate and armed B.
- `scripts/vm-test.sh` (`just vm-test`, `make vm-test`): release build →
  cached pinned rootfs and initramfs → candidate → private lab flowerlyd
  (Admin group = the invoking user's group) → create the candidate
  Generation and set its identity with the `build_id` baked into its slot →
  plan → `evolutiond lab run --record` → summary. Exit status 0 only for a
  recorded passing run.
- `test-build-task-parity.py` updated: `vm-test` is real; `vm` and `image`
  still fail closed.
- **Exit:** the recipe runs end to end on the KVM host.

### E5 — Real KVM acceptance

Issue: [#413](https://github.com/Doldas/flowerly_os/issues/413). Archive
tool: [#421](https://github.com/Doldas/flowerly_os/pull/421) (merged).

- After #416–#420 are merged, run the three ADR-0024 scenarios on
  `release/2`'s exact SHA against one reproducibility-lane candidate.
- `scripts/archive-lab-evidence.py archive` checks the three runs against
  the candidate and the SHA. It then archives the plans, results, serial
  logs and hashes under `reviews/evidence/0010-evolution-lab-<sha7>`, and a
  review note is added. `archive-lab-evidence.py check` re-verifies the
  archive.
- Done at `7ce5942`: [review](reviews/0010-evolution-lab-kvm-acceptance-2026-11-22.md),
  [evidence](reviews/evidence/0010-evolution-lab-7ce5942/).
- **Exit:** owner review of the evidence.

### E6 — Lanes and CI

Issue: [#414](https://github.com/Doldas/flowerly_os/issues/414). Merged PR:
[#420](https://github.com/Doldas/flowerly_os/pull/420).

- Upstream lane switch (D6) and its result marking.
- An opt-in `workflow_dispatch` job on the self-hosted KVM runner that runs
  `just vm-test` and uploads the result; not a required check until it has
  run cleanly on the runner.

## Deliberately out of scope

- Daemon-launched VMs (`evolution.run_experiment` stays unavailable; #95).
- Automatic activation, AI-selected promotion, fitness scoring.
- Distribution-grade image and installer (0.0.9).
