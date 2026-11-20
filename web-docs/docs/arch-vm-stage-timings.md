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

## Baseline and initial guest build cache (2026-11-17)

Measured on the green exact-SHA push run of `40ebfeb` (4 vCPU, 6 GiB guest):
the Arch step took 3193 s, of which `guest_verification` was 2928 s. Inside
the guest nothing is reused between runs: the dev build for clippy took 4 m 28 s,
`check` 51 s, the test profile 5 m 31 s and the release build 15 m 14 s, about
26 minutes of compiling in a fresh VM. Boot, upgrade, reboot and toolchain
install together are about 4 minutes.

`FLOWERLY_VM_BUILD_CACHE=1` (set for release pushes in `ci.yml`) first attached a
persistent sparse ext4 image (label `FLOWERLY_BUILD`, 28 GiB virtual) as a
second virtio drive. The guest mounted it, symlinked the checkout's `target/` to
it and kept `CARGO_HOME` there, so dependency artifacts and the downloaded
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

## Measured cold run

The [green push for `fd5e386`](https://github.com/Doldas/flowerly_os/actions/runs/44124162315)
reported a cold v2 cache, 8 vCPU and 12 GiB, with TCG and Arch kernel
`7.3.4-arch1-2`. Its stage ledger sums to 2,855 s. Guest verification took
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

### Matched KVM runs (2026-11-19)

Both local runs used `d6533714084ac39d1067adef0178880dec25afed`, the
same rehashed official Arch base image, fresh overlays, KVM, 4 vCPU and
6 GiB RAM. The cold run created a new cache after rotating the malformed
image described below. The next run reported `warm`. Both completed
`pacman -Syu`, rebooted into `7.3.5-arch1-1` and streamed fresh
exact-SHA success reports.

| Stage | Cold | Warm |
| --- | ---: | ---: |
| Base image acquisition/check | 1 s | 1 s |
| Overlay setup and VM launch | <1 s each | <1 s each |
| Initial boot/cloud-init | 90 s | 91 s |
| Pacman upgrade | 147 s | 17 s |
| Downloaded package archives | 351.76 MiB | 0 MiB |
| Post-upgrade reboot | 58 s | 49 s |
| Rust toolchain setup | 12 s | 7 s |
| Source transfer | 1 s | 1 s |
| Guest verification | 312 s | 182 s |
| Report transfer and shutdown | 2 s | 3 s |
| Total wall time | 623 s | 351 s |

The cold transaction reported 351.76 MiB of package downloads. With the
warm image pacman still synchronized repository databases and its
transaction listed no package downloads. The `release` compilation
reported 68 s cold and 0.07 s warm. The test-profile compilation
reported 16.77 s cold and 0.06 s warm. Both runs executed the full guest
test suite. Total wall time fell by 272 s (44%).

The [protected release push](https://github.com/Doldas/flowerly_os/actions/runs/44602528217)
began the Arch step on the same SHA. The self-hosted runner lost contact
with GitHub during the step and preserved no artifact for attempt 1.
The local KVM pair establishes the cache comparison. The protected
release acceptance still needs the rerun's fresh guest artifact.

### Real-guest cached disk failure (2026-11-19)

On `d6533714084ac39d1067adef0178880dec25afed`, a local KVM run
used an independently rehashed cached Arch cloud image and an isolated
build cache containing a 1 GiB zero-filled `build-cache-v3.raw` with no
dirty marker. The run directory contained a pre-existing `guest-report.txt`
with the same SHA and the success marker. The host classified the image as
`warm`, booted the guest and completed cloud-init and root geometry
checks. The guest then failed the cache mount with
`can't find LABEL=FLOWERLY_BUILD`. The host exited 1, reported the
pacman stage as interrupted, retained the dirty marker, and had no final
guest report. The next run reported `rotated-unclean` before booting.

To repeat, use fresh private directories for `ARCH_VM_BUILD_CACHE_DIR`
and `ARCH_VM_WORK_DIR`, enable `FLOWERLY_VM_BUILD_CACHE=1`, and provide
a previously verified cloud image. Seed the two files described above
and run the normal VM script with an unused `ARCH_VM_SSH_PORT`.

### Real-guest cached package failure (2026-11-19)

After the warm run shut down cleanly, the test zeroed the first block of
the cached `git-2.56.1-1-x86_64.pkg.tar.zst` with `debugfs punch`,
keeping the archive name, length and detached signature. A fresh KVM
overlay on the same SHA mounted the warm cache and synchronized its
repositories. Pacman reported an invalid PGP signature, refused to
commit the upgrade and installed no packages. The host exited 1, kept the
dirty marker and emitted no final guest report, despite a seeded
same-SHA success report in the run directory.
