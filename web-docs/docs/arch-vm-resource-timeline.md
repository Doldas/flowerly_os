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
