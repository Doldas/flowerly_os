# Generation IPC v1 typed argument validation (#53)

The `generation.create`, `generation.list`, `generation.inspect` and
`generation.activate` handlers accept JSON command argument **objects**.
Their known fields are checked by service-specific deserialization types
before database access; identifiers also undergo the existing
prefix/identity validation rather than accepting an arbitrary string.

For `generation.create`, `parent`, `reason_goal` and `experiment`
are optional string identifiers; `source_commit` and
`system_manifest` are optional strings. **Missing and explicit null**
continue to mean absent values. Numeric, boolean, array, invalid-prefix
or malformed known fields fail as `invalid_argument`.

`generation.promote` (Admin, #225 PR 6) requires `id` and a string
`reason` of 1..512 bytes that is not blank; anything else is
`invalid_argument`. Promoting a Generation that is already `system`, or
is not `CANDIDATE`/`TESTED`, is `conflict`.

On `generation.create`, a present `source_commit` or `system_manifest`
must be 1..256 visible ASCII characters. They are validated, not trusted:
`TESTED` needs the complete identity below.

The lifecycle commands of
[ADR-0023](adr/0023-generation-lifecycle-and-durable-activation.md) are
Admin only and attributed to the kernel-attested caller. `reason` is
1..512 bytes and not blank wherever it appears.

| Command | Required | Optional |
| --- | --- | --- |
| `generation.identity` | `id`, `source_commit` (40 hex), `rootfs_sha256`, `kernel_sha256`, `initramfs_sha256`, `input_manifest_sha256`, `build_id`, `reason` | |
| `generation.evidence` | `id`, `outcome` (`passed`/`failed`), `evidence_sha256`, `reason` | |
| `generation.test` | `id`, `experiment`, `reason` | |
| `generation.reject`, `generation.revoke` | `id`, `reason` | |
| `generation.activate`, `generation.rollback` | `id` | `reason`, `operation_id`, `acknowledges` |
| `generation.history` | | `id` |
| `generation.resolve` (service account) | | |

`generation.evidence` returns the Experiment to pass to `generation.test`.
Recording the same `evidence_sha256` and `outcome` again returns the same
Experiment; the same hash with another outcome is `conflict`. Only a
`passed` Experiment of that Generation can back `TESTED`. An illegal
lifecycle step is `conflict`.

`generation.activate` and `generation.rollback` validate their arguments
and then refuse with `conflict`, writing nothing, until Gate D is verified.

`generation.resolve` resolves the pending activation, if any, from the boot
ledger and returns its `outcome`: `none`, `waiting`, `unresolved` (with a
`reason`; the request stays pending), `confirmed`, `fell_back` or
`abandoned`. With nothing pending it does not read the ledger.

Inspect, activate and rollback require `id` to be a valid Generation ID. Unknown
object keys continue to be ignored to preserve forwards compatibility
with existing IPC v1 clients. Unknown command names still return
`invalid_argument`; missing referenced objects return `not_found`.
SQLite busy remains `unavailable`, known unique conflicts
`conflict`, and corruption/other SQLite failures `internal`.

No IPC envelope or protocol version has changed. This is an
incremental Generation command contract only; remaining commands and
domain services still need the full #53 audit and negative tests.
