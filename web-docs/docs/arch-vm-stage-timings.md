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

## Remaining work before #285 is complete

- Capture cold and warm exact-SHA runs on the authorized self-hosted
  runner and compare total durations with the prior baseline.
- Measure guest compile and guest tests independently, and collect
  cache hit/miss, transferred bytes and request counts.
- Verify cache lifetime and ownership on the runner, design trusted
  package/toolchain caching with signature verification, bounded age,
  and cold fallback. No new cache has been enabled by this PR.
- Repeat the full real Arch guest verification and validate release
  gates against the exact commit. Do not infer speedup or release
  acceptance from instrumentation alone.

Status 2026-11-07: phase-1 host instrumentation proposed;
benchmarks, cache implementation, and real VM evidence pending.
