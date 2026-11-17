# Arch VM stage timing evidence (#285)

Every `scripts/arch-kernel-vm-test.sh` run now creates
`target/arch-kernel/<slug>/report/host-stage-timings.tsv`, alongside its
existing host-resource and guest-execution evidence. The file is non-secret
and records `stage`, `duration_seconds`, and `outcome`
(`completed` or `interrupted`). The early EXIT trap retains partial
timings when image acquisition or VM setup fails; the later VM cleanup
trap records the last stage before terminating the VM.

Stages are host preflight, image acquisition (including SHA retrieval and
verification), overlay setup, VM launch, first boot/cloud-init, pacman
upgrade, reboot, Rust toolchain installation, source archive/transfer,
guest verification (compile and tests combined), guest report retrieval,
and guest shutdown. Values are whole-second elapsed wall time: suitable
for comparing stages, not CPU time or fine-grained profiling.

No guest credentials, environment secrets, prompts or guest home files
are emitted by this ledger. The exact test commit remains in the
existing host metadata/report files. A stage interrupted by failure is
**not** a successful release acceptance.

## Validation

`bash scripts/test-arch-vm-stage-timing.sh` tests successful and
interrupted records and duplicate-finish suppression. It is included
in `bash scripts/preflight-docs.sh`.

## Baseline and the guest build cache (2026-11-17)

Measured on the green exact-SHA push run of `40ebfeb` (4 vCPU, 6 GiB guest):
the Arch step took 3193 s, of which `guest_verification` was 2928 s. Inside
the guest nothing is reused between runs: the dev build for clippy took 4 m 28 s,
`check` 51 s, the test profile 5 m 31 s and the release build 15 m 14 s, about
26 minutes of compiling in a fresh VM. Boot, upgrade, reboot and toolchain
install together are about 4 minutes.

`FLOWERLY_VM_BUILD_CACHE=1` (set for release pushes in `ci.yml`) attaches a
persistent sparse ext4 image (label `FLOWERLY_BUILD`, 28 GiB virtual) as a
second virtio drive. The guest mounts it, symlinks the checkout's `target/` to
it and keeps `CARGO_HOME` there, so dependency artifacts and the downloaded
registry survive between runs. cargo's fingerprints decide what is reused, so
a hit cannot make a stale build pass. The image lives under
`~/.cache/flowerly/arch-vm-build` on the runner, is used by one VM at a time
(a second run goes cold, reported as `locked`), and is recreated when it is
14 days old or 20 GiB allocated. `build_cache=<state>` is recorded in
`host-metadata.txt`. Pure acceptance is unchanged; only compile time is saved.
`bash scripts/test-arch-vm-build-cache.sh` covers the states offline.

The release-push guest is also sized up from 4 vCPU / 6 GiB to 8 vCPU / 12 GiB
(`ARCH_VM_CPUS`, `ARCH_VM_MEMORY_MB` in `ci.yml`; both are recorded in
`host-metadata.txt`), so the first run changes two things at once. Read the
cold run (new size, empty cache) against the 3193 s baseline and the warm run
against the cold one.

The first run after enabling is cold. The saving is **not yet measured**: the
first two exact-SHA release pushes give the cold and warm numbers, which belong
in the list below.

## Remaining work before #285 is complete

- Capture cold and warm exact-SHA runs on the authorized self-hosted
  runner and compare total durations with the prior baseline.
- Measure guest compile and guest tests independently, and collect
  cache hit/miss, transferred bytes and request counts.
- Package caching in the guest (pacman) is not done; the cargo cache above
  covers compilation only. A trusted pacman cache would need signature
  verification, bounded age and a cold fallback.
- Repeat the full real Arch guest verification and validate release
  gates against the exact commit. Do not infer speedup or release
  acceptance from instrumentation alone.

Status 2026-11-07: phase-1 host instrumentation proposed;
benchmarks, cache implementation, and real VM evidence pending.
