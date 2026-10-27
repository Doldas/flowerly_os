# IPC v1 typed command arguments (#53)

Status: **every service audited.** Generation (#205), Flow, Goal and
Session metadata use typed arguments. Execution, Provenance and Compute
validate each known field explicitly (null means absent); the Compute
inventory is [below](#compute-commands-explicit-validation). The
malformed-argument regression from #213 keeps all of them returning typed
error codes.

## Contract for migrated commands

Shared helper: `src/flowerlyd/src/args.rs`. Each command deserializes its
known fields into a typed struct before any database access.

- Arguments must be a JSON **object**. Anything else is `invalid_argument`.
- A **missing** field and an explicit **`null`** both mean "absent".
- A known field of the **wrong JSON type** is `invalid_argument`. Values are
  never coerced or silently dropped.
- **Unknown fields are ignored**, so older and newer IPC v1 clients keep
  working. `created_by` is always overwritten with the kernel-attested peer.
- Typed IDs also pass their prefix validation (`flow-…`, `goal-…`).
- Missing referenced objects are `not_found`. Storage failures follow the
  shared classifier (`ipc-mutation-outcomes.md`).

The envelope and `PROTOCOL_VERSION` are unchanged.

## Migrated commands

| Service | Commands | Typed fields |
| --- | --- | --- |
| Generation | `generation.create`, `.list`, `.inspect`, `.activate` | See [Generation arguments](generation-ipc-v1-arguments.md). |
| Flow | `flow.create` | `name` (string, required), `operation_id` (string) |
| Flow | `flow.activate`, `flow.archive` | `id` (Flow ID) or `name` (string); `id` wins; `null` is absent |
| Flow | `flow.list` | none |
| Goal | `goal.create` | `objective` (string, required), `priority` (integer in i32 range), `rationale` (string), `operation_id` (string) |
| Goal | `goal.child` | `id` (Goal ID), `objective` (string), `operation_id` (string) |
| Goal | `goal.show`, `goal.complete`, `goal.abandon` | `id` (Goal ID) |
| Goal | `goal.list` | none |
| Session | `session.create` | `flow_id` (Flow ID), `pid` (integer in i32 range), `kind` (string, default `local-pty`), `cwd` (string), `operation_id` (string) |
| Session | `session.terminate` (metadata path) | `id` (Session ID) |
| Session | `session.list` | none |

## Behavior changes from the untyped handlers

- `goal.create` with a non-string `rationale` is now `invalid_argument`.
  Before, the rationale was silently dropped and the Goal created anyway.
- `session.create` with a non-string `kind` or `cwd` is now `invalid_argument`.
  Before, `kind` silently became `local-pty` and `cwd` was dropped.
- `flow.activate` / `flow.archive` with `"id": null` now fall back to `name`.
  Before, this was refused with "arg `id` must be a string".

## Compute commands (explicit validation)

Compute validates each field by hand rather than through a typed struct,
with the same contract: a wrong JSON type is `invalid_argument`, absent and
`null` are the same, unknown fields are ignored. Audited 2026-10-25 (#53).
"Default" means the value used when the field is absent or `null`; it is
never applied to a wrongly typed value.

| Command | Fields |
| --- | --- |
| `model.register` | `name` (string, 1..256, required); `backend` (known backend name; default `unknown`); `context_tokens`, `measured_memory_bytes` (positive integer, floats refused); `family`, `architecture`, `license` (≤ 256), `evidence` (≤ 512) (non-blank string); `modalities` (≤ 16 strings of 1..32 characters, lowercased and de-duplicated); `operation_id` |
| `model.show`, `model.retire`, `model.compatibility` | `id` (Model ID) |
| `model.list`, `device.budget`, `profile.list`, `provider.list`, `inference.capabilities` | none |
| `profile.create`, `profile.import` | `name` (string, 1..128); `toml` (string ≤ 16 KiB, credential-free, validated); `operation_id` |
| `profile.clone` | `id` (Profile ID); `name`; `operation_id` |
| `profile.rename` | `id`; `name`; `operation_id` (validated; a rename is idempotent by itself, so the key is not recorded) |
| `profile.edit` | `id`; `toml`; `operation_id` |
| `profile.show`, `profile.export`, `profile.resolve` | `id`; `version` (integer 1..u32::MAX; default latest) |
| `profile.version_history` | `id` |
| `provider.register` | `name`; `kind` (string; default `local-unix`; any other kind is `permission_denied`); `endpoint` (bare filename); `capabilities` (1..16 strings); `operation_id` |
| `provider.show` | `id` (Provider ID) |
| `workload.submit` | `profile_id`, `model_id` (IDs, required); `background` (`quiet` / `normal` / `intensive`; default `normal`); `priority` (integer in i32 range; default 0); `operation_id` |
| `workload.list` | `mine` (boolean; default `true`; `false` is `invalid_argument` since #225 PR 5, because only the caller's own workloads are listed) |
| `workload.show`, `workload.stop`, `workload.complete` | `id` (Workload ID) |
| `workload.usage_brief` | `model_id` |
| `usage.record` | `workload_id`; `tokens_in`, `tokens_out`, `latency_ms`, `cost_micros` (required integers ≤ 4 × u32::MAX); `operation_id` **refused** |
| `inference.request` | `workload_id`, `provider_id` (IDs); `prompt` (string ≤ 16 KiB); `max_tokens` (1..4096; default 128); `request_id` (required idempotency key); `operation_id` |

`operation_id` everywhere follows the idempotency key rules: 1..128 ASCII
letters, digits, `.`, `_`, `:` or `-`. An exact retry replays the recorded
response, and the same key with different arguments is `conflict`.

### Defects fixed by the audit

- `profile.clone` and `provider.register` recorded their operation with
  empty request and response payloads. A retry with the same key skipped the
  write but returned a **newly generated id that was never stored**, and a
  reused key with different arguments replayed instead of returning
  `conflict`. Both now record and replay the real request and response.
- `profile.edit` validated `operation_id` and then discarded it, so a retry
  appended a duplicate version. An exact retry now replays the recorded
  version.
- `profile.rename` accepted any `operation_id` without validation.
- `usage.record` silently ignored `operation_id`, so a retried ledger write
  counted twice. It now refuses the key with `invalid_argument` rather than
  promise retry safety it does not provide.
- `workload.list` refused `"mine": null`; it now means absent (`true`).
- `inference.request` silently assumed the `ask` fallback when the
  workload's profile could not be resolved. It now returns that error.
- `inference.request` recorded a missing provider `latency_ms` as a measured
  zero. A missing value is now an `internal` provider-contract error, like
  missing token counts.

### Known limits

- `workload.stop` and `workload.complete` take no `operation_id`. A retry
  after an unknown outcome gets `conflict` for the repeated transition
  rather than the original success (#63).
