# Local model catalog and measured device budget (RFC 0004 tranche)

Status: **prototype surface, first tranche of RFC 0004**; no provider, no
network, no scheduling claims.

## What exists

Flowerly OS keeps a durable, provenance-tracked **local model catalog** in the
shared SQLite state (`models` table, migration 0006). It records only
**measured or explicitly recorded** metadata:

| Field | Meaning |
|---|---|
| `backend` | Recorded execution substrate (CPU/CUDA/VULKAN/ROCM/NPU/UNKNOWN). |
| `measured_memory_bytes` | A recorded measurement (e.g. gold RSS). NULL = unknown. |
| `context_tokens` | Recorded context limit. NULL = unknown. |
| `modalities` | Lowercased set of accepted modality labels. |
| `license` / `evidence` | License label and the measurement/evidence origin. |

Unknown values are **never estimated or substituted**. Compatibility checks
report `UNKNOWN` with explicit reasons when evidence is missing, `FITS` or
`EXCEEDS_BUDGET` against a **measured** device budget, and label every verdict
`advisory-only`. `/proc/meminfo` is the only numeric source; GPU presence is a
DRM render-node observation only; per-process VRAM quotas are labeled
`not-enforced`.

## Command surface

| Command | Behavior |
|---|---|
| `model.register` | Strict validation; optional `operation_id` gives **unknown-outcome replay safety** (#63 semantics). |
| `model.list` / `model.show` | Active and full (incl. retired) views. |
| `model.retire` | Typed soft retirement; provenance edge, no deletion. |
| `device.budget` | Measured memory + honest enforcement labels. |
| `model.compatibility` | Fielded verdict with reasons and promise labels. |

Typed errors: invalid metadata → `InvalidArgument`; unknown id → `NotFound`;
replay conflict → `Conflict`; SQLite busy → `Unavailable`.

## Security boundaries (RFC 0004 invariants)

- The catalog **cannot grant OS authority**: no command spawns, opens files or
  touches network. Model/profile selection is preference data only.
- No provider, endpoint or credential enters this module; remote provider
  policy does not exist yet and defaults to **no egress**.
- Local-only tranche: `device.budget` reads `/proc/meminfo` and
  `/sys/class/drm` on a read-only basis.
- Registrations create durable provenance edges (`created_by`, retirement
  `caused_by`) using the trusted kernel-verified actor label.

## Honest limitations

- No GPU/VRAM measurement or enforcement is claimed; the budget is CPU
  memory advisory data.
- No benchmark scores, task capability measurement or model fit quality is
  inferred from names or parameter counts. Evidence fields are labels, not
  judgments.
- No inference execution happens in this module: the catalog describes
  candidates; scheduling is a separate, later gated tranche.
