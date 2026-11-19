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

**Defect found by the first cache run (`51f6e6b`).** The run before it
(`4d2c80c`) had the host's QEMU segfault mid-build; the next run reused the cache
image and rustc failed with `internal compiler error: failed to load disk-cached
value for green node` (incremental compilation data torn by the crash). The
failure was real but caused by the cache, not by the code. The cache now (a)
turns incremental compilation off in the guest (`CARGO_INCREMENTAL=0`), (b)
marks the image dirty when a run starts and clean only when the run passed and
the VM powered off by itself, rotating a dirty image (`rotated-unclean`), and
(c) uses a versioned file name (`build-cache-v2.raw`) so the image written
under the old rules is deleted, never reused.

The release-push guest is also sized up from 4 vCPU / 6 GiB to 8 vCPU / 12 GiB
(`ARCH_VM_CPUS`, `ARCH_VM_MEMORY_MB` in `ci.yml`; both are recorded in
`host-metadata.txt`), so the first run changes two things at once. Read the
cold run (new size, empty cache) against the 3193 s baseline and the warm run
against the cold one.

The first run after enabling is cold. The saving is **not yet measured**: the
first two exact-SHA release pushes give the cold and warm numbers, which belong
in the list below.

## Measured cold run

The [green push for `fd5e386`](https://github.com/Doldas/flowerly_os/actions/runs/44124162315)
reported a cold v2 cache, 8 vCPU and 12 GiB, with TCG and Arch kernel
`7.3.4-arch1-2`. Its stage ledger sums to 2,854 s. Guest verification took
2,577 s, pacman took 64 s, and first boot plus reboot took 179 s.
Compared with the earlier 2,928 s guest verification, the observed
difference is 351 s. The VM sizing changed and this cache was empty.

## Cache v3

The trusted release-push runner uses one image for the Cargo registry,
the debug and release build profiles, and a root-owned pacman archive
directory. Manual dispatch uses the guest's local cache. The image is
rotated if the previous run did not finish and unmount it cleanly, if its
modification time is at least 14 days old, or if its allocation reaches
20 GiB. Older layouts are discarded. The host refuses a shared cache
directory writable by group or other users.

The guest still refreshes its repositories, validates package signatures
and completes the full `pacman -Syu` transaction before the mandatory reboot.
A failed transaction fails the run and leaves the cache dirty for rotation.
Runtime state and reports stay off the cache disk. `guest-report.txt` comes
from this invocation's SSH stream, with the expected commit and the final
success marker. `guest-pacman.log` records pacman's download summary, and
the guest verification log has wall times for Clippy, compile check,
the Rust test command and release build. The Cargo test-profile compile
line can be compared with the Rust test command's wall time.

Collect consecutive cold and warm v3 runs with identical guest sizing.
Compare host timings, the download sizes in `guest-pacman.log`, and the
guest compiler and test timings. Confirm that a corrupt archive or an
interrupted run cannot produce accepted evidence. Until these runs are
available, the package reuse and its speedup remain unmeasured.
