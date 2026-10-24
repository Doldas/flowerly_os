# IPC v1 typed command arguments (#53)

Status: **migrating service by service.** Generation (#205), Flow, Goal and
Session metadata use typed arguments. Execution and Provenance validate each
known field explicitly (null means absent). Compute still validates untyped
JSON by hand. The malformed-argument regression from
#213 nevertheless keeps all of them returning typed error codes.

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
