# Arch release-VM resource timeline

Issue: [#184](https://github.com/Doldas/flowerly_os/issues/184).

The exact-SHA Arch guest acceptance script collects a bounded
`host-resource-timeline.csv` while QEMU is running. It samples host
available memory and swap, the QEMU resident set size and process
presence, and readable cgroup-v2 memory counters every 10 seconds,
up to 1,000 samples. This is **observability, not an OOM fix**.

The report lives under
`target/arch-kernel/release-push/report/host-resource-timeline.csv`
and is uploaded with the existing Arch evidence artifact, including
when required tests fail. A separate `host-resource-exit.txt` records
the final cleanup snapshot. Missing data are blank, **not zero**.

To compare a failed attempt, inspect the final timestamped samples,
changes in `cgroup_oom` and `cgroup_oom_kill`, whether QEMU
disappeared, and available host memory/swap. These counters do not
alone establish root cause. Guest OOM, host OOM, QEMU exit and SSH
disruption still require independent corroboration. Kernel journals
and credentials are deliberately not copied into CI artifacts.

This monitor must never alter root-refusal, delegated cgroup
containment, guest acceptance, or exact-commit release checks.
An unavailable monitor is a diagnostic limitation, not permission
to claim successful release verification.

## Failure classification (#184)

Every run now writes two more files to the same artifact:

- `host-resource-start.txt` records, at the start: the host boot ID and boot
  time, CPU count, memory and swap, the runner cgroup's `oom_kill` counter,
  and whether `/dev/kvm` is usable.
- `failure-classification.txt` is written by
  `scripts/arch-vm-failure-classify.py` from the facts the script recorded:
  exit status, the stage that was running, the guest SSH exit code, QEMU
  state, a fresh SSH probe after a failure, the `oom_kill` change and the boot
  ID. It also includes at most 20 printable QEMU serial console lines that
  match guest OOM or crash patterns.

| Category | Evidence |
| --- | --- |
| `success` | Exit status 0. |
| `host_reboot` | The host boot ID changed during the run. |
| `host_oom` | QEMU is gone and the runner cgroup's `oom_kill` grew. |
| `qemu_failure` | QEMU is gone with no host OOM kill. |
| `guest_oom` | The guest kernel logged an OOM kill on the serial console. |
| `guest_crash` | Panic, BUG, lockup or watchdog lines on the console, or QEMU alive while the guest no longer answers SSH. |
| `ssh_interruption` | `ssh` exited 255 but the guest still answers SSH. |
| `test_failure` | A guest command failed (exit code other than 255) and the guest still answers SSH. |
| `unknown` | None of the above. |

The classification is observational. It never changes a result, and it never
copies journals, environment variables, SSH material or guest files. Offline
fixtures: `scripts/test-arch-vm-failure-classify.py` (CI `docs` job and
`preflight-docs.sh`).

A **host reboot that kills the runner** cannot be recorded by the run it
interrupts. GitHub reports "The self-hosted runner lost communication with the
server". Confirm it from the next run's `host_boot_time_utc` in
`host-resource-start.txt`, or on the host with `journalctl --list-boots`.

## Supported runner configuration (measured 2026-10-26)

Three consecutive exact-SHA runs passed, on `29c9e88`, `1a58a30` and
`afaaa66`, all with QEMU `tcg`, 4 vCPUs and 6144 MiB of guest memory:

| Measure | Observed |
| --- | --- |
| Duration | 2180–2230 s |
| Peak QEMU RSS | 6221–6290 MiB |
| Peak runner cgroup memory | 7837–10274 MiB |
| Minimum host `MemAvailable` | 6304–9362 MiB |
| Minimum host free swap | 11222–14726 MiB (of 16 GiB) |
| Runner cgroup `oom_kill` | 0 |

Required for the release gate, derived from these measurements with headroom.
These are minimums, not guarantees:

- **Memory:** 16 GiB available to the runner (6 GiB guest + 0.3 GiB QEMU
  overhead + up to 10 GiB for the host-side build and tests), and 4 GiB of free
  swap.
- **CPU:** 4 vCPUs for the guest, plus at least 2 for the host side.
- **Concurrency:** **one** Arch VM acceptance at a time per host. The single
  `flowerly-runner` serializes this repository's jobs. Other runners or
  memory-heavy services on the same host (as on `gamitron-server`) count
  against the same RAM and must not run concurrently with peak memory use.
- **Accelerator:** the runner container has no `/dev/kvm`, so runs use `tcg`
  (about 36 min). Passing `/dev/kvm` to the runner container is a host
  decision that would shorten runs; it is not required for acceptance.

## Known infrastructure event: unexplained host reset (2026-10-26)

Push CI run 40335577209 (`c6ec870`) failed at "Release build" with "runner lost
communication". The host (`gamitron-server`) journal ends at 14:03:24 UTC
**without a shutdown sequence**, and a new boot starts at 14:04:43 UTC. That is
an unclean reset. No OOM, panic or thermal message was found in the preserved
journal. **The cause is unexplained**, whether power, hardware, firmware or an
unlogged kernel crash. Impact on release confidence:

- The failure was not a code or test failure. The same tree passed on the
  following exact SHAs.
- It shows that the shared host can reset under build load. Until the cause is
  known, a release candidate needs a **completed** exact-SHA run. A missing
  run is never treated as passed, and evidence is never reused from another
  SHA.
