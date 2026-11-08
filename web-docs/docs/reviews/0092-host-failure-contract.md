# #92 — typed telemetry driver failures

Status 2026-11-07: **safe incremental host adapter test**. The existing `flowerlyd::compute::host` remains the actual read-only Linux /proc and DRM adapter; an explicit `HostReadFailure` type distinguishes unavailable, permission denied, interruption/timeout and other I/O. Unit tests check classification, while existing budget tests keep unavailable data unknown rather than fabricating zero telemetry.

No privileged cgroup/process allocation or mount API was introduced. Full typed capability/owner-bound cgroup facade, PID reuse/namespace adversarial tests, real delegated Arch guest, and trusted supervisor lifecycle remain under #92/#95 and require explicit security owner approval. Runtime authority/PTY safety is unchanged.
