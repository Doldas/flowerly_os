# Workload admission and usage ledger (RFC 0004 tranche 3)

Status: **prototype surface, third tranche of RFC 0004**; builds on the
[local model catalog](compute-catalog.md) and
[expert profiles and the provider registry](compute-profiles.md).
**No execution, no process control, no network.** ADR-0020 records the
process-boundary decision (module, not daemon).

## What admission is — and is not

`workload.submit` records an **admission decision** against the resolved
profile ceilings and the model catalog evidence. It is honest fail-closed
bookkeeping:

- The model must carry a **measured** memory footprint; missing evidence is
  `REFUSED` with the reason, never a guess.
- **GPU/accelerator backends (CUDA/VULKAN/ROCM/NPU) are refused** in this
  tranche: no verified VRAM measurement or enforcement exists yet, so
  admitting them would be a false security claim.
- Effective profile ceilings apply: `memory_max_bytes` vs the model's
  measured footprint; `concurrency_max` per profile per submitting actor —
  exhausted ceilings **queue** the workload (`QUEUED`) unless the profile's
  fallback is explicitly `fail`, which refuses instead.
- Linux **memory pressure** (PSI `avg10` from `/proc/pressure/memory`) above
  a fixed hold threshold (50%) refuses admission; a missing PSI source is
  reported as unknown, never as 0.
- Every decision row snapshots the **resolved ceilings** and the **reason
  string**; `REFUSED` decisions are recorded, so accounting stays auditable.

## Ledger

`usage.record` appends a token/latency/cost ledger entry
(`inference_usage`, migration 0008). Totals per model come back as advisory
summaries (`workload.usage_brief`). No provider executes anything in this
tranche — the ledger expects entries from a later authorized provider
boundary; nothing fabricates usage rows itself.

## Command surface

| Command | Behavior |
|---|---|
| `workload.submit` | Fail-closed admission; `operation_id` replay-safe (#63). |
| `workload.list` / `workload.show` | Decision list/details incl. ceiling snapshot. |
| `workload.stop` / `workload.complete` | Typed terminal transitions from QUEUED/ADMITTED; provenance edges. |
| `usage.record` | Ledger row; typed validation; NotFound for unknown workloads. |
| `workload.usage_brief` | Per-model token/cost totals with `promise: ledger-only`. |

## Security boundaries (RFC 0004 invariants)

- Admission is **preference-data-only** (`promise: preference-data-only` in
  resolves; ledger labeled); no workload admits itself into any OS
  capability. No process, cgroup or network is touched by these commands.
- No credentials, endpoints or egress exist in this module.
- Background activity is recorded per workload; the capability tier is part
  of the snapshot only. One advanced request never implies unlimited
  background agents: concurrency ceilings are per-profile.

## Honest limitations

- Enforcement today is **bookkeeping + refusal logic**, not OS-level
  enforcement of inference processes; cgroup/GPU enforcement requires the
  verified-evidence gates in RFC 0004 and is a future tranche.
- No scheduler preemption, priority-based eviction or OOM recovery is
  implemented; priority is recorded and used for list ordering only.
- The PSI threshold is a constant, not a tuned policy; multi-tenant cost
  attribution beyond the actor label does not exist yet.
