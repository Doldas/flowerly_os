# ADR-0024: Evolution Lab experiment pipeline and authority

**Status:** Proposed 2026-11-21 for 0.0.6. The decisions D1–D6 below are the
defaults the implementation follows; the owner accepts or changes them in
review. **Issues:** [#10](https://github.com/Doldas/flowerly_os/issues/10)
(roadmap), [#3](https://github.com/Doldas/flowerly_os/issues/3) (vertical
slice). **Scope:** 0.0.6. Builds on
[ADR-0007](0007-kvm-evolution-world.md) (KVM Evolution World),
[ADR-0019](0019-generation-rootfs-rollback-proposal.md) (A/B slots, boot
counting, witness), [ADR-0020](0020-pinned-arch-generation-inputs-proposal.md)
(pinned inputs) and [ADR-0023](0023-generation-lifecycle-and-durable-activation.md)
(lifecycle, evidence, activation).

## Context: what exists after 0.0.5 (verified on `release/2@57e078e`)

1. **`evolutiond` is a stub.** It answers `evolution.experiment-status` with
   `unavailable` and refuses `evolution.run_experiment`
   (`src/evolutiond/src/main.rs`). Its unit already runs it as
   `flowerly-evolution` with `SupplementaryGroups=kvm` and
   `DeviceAllow=/dev/kvm rw`, but `kvm::PROVISIONED` is `false`.
2. **Experiments exist only as evidence rows.** `generation.evidence` (Admin)
   inserts an `experiments` row with `hypothesis='generation acceptance'`,
   `metrics={"evidence_sha256":…}` and `result` `passed|failed`;
   `generation.test` moves `CANDIDATE → TESTED` only with a passing one. The
   table's `baseline`, `test_plan` and `decision` columns are never written,
   nothing writes `artifacts`, and no command reads an Experiment back.
3. **Real KVM runs are separate Python harnesses.** S7
   (`generation-ab-kvm-acceptance.py`) and Gate D
   (`generation-gate-d-kvm-acceptance.py`) boot the Flowerly A/B fixture under
   OVMF/KVM, one QEMU process per boot, and read markers or a base64 report
   from the serial console. They are acceptance tools for one milestone each,
   not a reusable pipeline, and they record nothing in Flowerly's state.
4. **The candidate build is manual.** `build-g5-test-image.sh` builds slot
   images from the pinned rootfs (ADR-0020, frozen set `8689cd41…`), but the
   rootfs, initramfs and package cache are prepared by hand.
5. **`just vm-test` and `just vm` fail closed** (`exit 1`), and
   `scripts/test-build-task-parity.py` asserts that.
6. **No privileged KVM in a daemon yet.** The Layer 1.5 review (#95) requires
   real containment evidence before a daemon gains KVM/mount authority
   (`layer-15-pty-kvm-threat-review.md`).

## Decision

### D1. The lab runs as `evolutiond lab`, as the invoking user; the daemon stays unprovisioned

The pipeline is a subcommand of the `evolutiond` binary, run in the
foreground by an operator or CI account that already has access to
`/dev/kvm`: `evolutiond lab run|verify|record`. It never escalates (no `sudo`,
no setuid helper, no root fallback), launches QEMU with no network
(`-net none`), no host directory sharing and no monitor, and keeps every file
it writes inside its output directory. The daemon's
`evolution.run_experiment` keeps refusing as `unavailable` in 0.0.6: moving VM
launches into the long-running service is a privilege change for #95.

The roadmap's `evolution/vm`, `evolution/benchmarks` and `evolution/promotion`
are modules of the `evolutiond` crate (`vm`, `benchmarks`, `promotion`), as
ADR-0019 (two-daemon layout) places all evolution code there.

### D2. The candidate is the 0.0.5 fixture, built from the frozen inputs

A candidate is a Flowerly Generation root built from the current source tree
and the **frozen package set**: the pinned rootfs, the release binaries, the
test-profile units, a baked identity v2 and the boot payload, on a two-slot
disk made by `build-flowerly-ab-disk.sh`. Package, kernel and Flowerly
identities are checked twice: on the host before boot (`distro-builder
manifest verify-rootfs` / `verify-binding`) and **inside the booted guest**
against the result (D4). Rolling Arch inputs are a separate, opt-in
*upstream lane* (D6), never the default.

### D3. Promotion stops at `TESTED` or `REJECTED`

The `promotion` module turns check results into a verdict with one fixed
policy: every check of the plan must be present and pass; a missing,
malformed or unparsable check fails the run (fail closed). A passing run
recommends `generation.test` (`CANDIDATE → TESTED`); a failing run recommends
`generation.reject`. The lab applies the recommendation only with
`--record`, through flowerlyd as the operator's Admin identity. **It never
activates.** `STABLE` remains a witnessed, human-requested activation
(ADR-0023 §2). No AI or automatic path reaches `generation.activate`.

### D4. A test-profile probe reports from inside the guest; the host decides

A one-shot unit installed only by the test-profile script (like the Gate D
drill) runs after `flowerlyd` and the health gate on every lab boot. It
gathers facts and runs Admin checks through the real
`agenticctl → flowerlyd` path, writes one report to the console
(base64 JSON in numbered lines and a final line with its SHA-256, the Gate D
format) and powers the guest off. It does not judge: the host compares the
facts with what it built (identity, root PARTUUID, kernel hash, package list,
binary hashes) and computes every verdict itself. The probe and its Admin
account never ship in the release image.

### D5. Experiments become complete records; artifacts are content-addressed

`generation.evidence` gains an optional `experiment` object — `hypothesis`,
`baseline` (an existing Generation), `test_plan` (check names), numeric
`metrics` and `artifacts` (`kind`, `name`, `sha256`, `bytes`) — written to the
existing columns. Each artifact becomes an `artifacts` row with the URI
`sha256:<hex>` and an `experiment produced artifact` provenance edge; the lab
output directory holds the bytes, and `evolutiond lab verify` rehashes them.
`evidence_sha256` is the SHA-256 of the canonical result document, which
lists every artifact hash, so the record and the files bind each other.
`generation.test` and `generation.reject` set the Experiment's `decision`
(`tested` / `rejected`) and the rejection may name the Experiment. New
read-only commands `experiment.inspect` and `experiment.list` (Admin, like
`generation.history`) return the record. No schema migration is needed: all
columns exist since 0001.

### D6. Two lanes

- **Reproducibility lane (default, `just vm-test`):** frozen package set,
  locked Cargo dependencies, recorded tooling. A change in the result means a
  change in Flowerly.
- **Upstream lane (opt-in, `FLOWERLY_LAB_LANE=upstream`):** the rootfs is
  resolved from the current Arch repositories on the run day and the result
  records the resolved package list instead of claiming the frozen manifest.
  It finds upstream breakage even when Flowerly is unchanged. It is never
  evidence for `TESTED` in the reproducibility sense; its experiments say
  `lane: upstream`.

## Scenarios the lab must prove on a real KVM host

1. **Good candidate:** build → boot → identity/package/kernel checks → health
   → integration → clean shutdown → result → Experiment `passed` → `TESTED`.
2. **Bad candidate:** the same candidate with an injected failure (the 0.0.5
   `flowerly.test.fail_health=1` option on its entry) → health fails → result
   `failed` → `REJECTED`, with the evidence of why.
3. **Rollback after a bad candidate:** baseline A boots and is healthy; the bad
   candidate B is armed with boot counting; each boot of B is reported
   unhealthy and left unblessed; once its tries are spent the loader returns
   to A, whose probe reports the booted root A healthy. The Experiment for B is
   `failed`, and the result carries the observed fallback.

## Consequences

- One reusable pipeline replaces milestone-specific harnesses for new work;
  the S7 and Gate D harnesses stay as the 0.0.5 acceptance record.
- KVM runs remain an operator/CI action on a host with `/dev/kvm`; nothing in
  the Stable World starts VMs on its own.
- The probe and lab Admin account widen the **test profile** only.
- Experiments become inspectable records with metrics and artifacts, which
  0.0.7 (Goal runtime) can link to Goals.

## Explicitly not in 0.0.6

- Daemon-initiated VM launches, VM networking, host directory sharing.
- Automatic activation or any AI-chosen promotion.
- Multi-dimensional fitness scoring; benchmarks are recorded, not judged
  beyond the plan's fixed limits.
- Bit-for-bit reproducible images (inputs are pinned, outputs are hashed).
