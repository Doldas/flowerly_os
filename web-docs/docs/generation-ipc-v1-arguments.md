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
| `generation.evidence` | `id`, `outcome` (`passed`/`failed`), `evidence_sha256`, `reason` | `experiment` (object, below) |
| `generation.test` | `id`, `experiment`, `reason` | |
| `generation.reject` | `id`, `reason` | `experiment` (Experiment id) |
| `generation.revoke` | `id`, `reason` | |
| `generation.activate`, `generation.rollback` | `id`, `reason` | `operation_id`, `acknowledges` (activate), `acknowledge_irreversible` (bool) |
| `generation.history` | | `id` |
| `generation.resolve` (service account) | | |

`generation.evidence` returns the Experiment to pass to `generation.test`.
Recording the same `evidence_sha256` and `outcome` again returns the same
Experiment; the same hash with another outcome is `conflict`. Only a
`passed` Experiment of that Generation can back `TESTED`. An illegal
lifecycle step is `conflict`.

## Experiment records (ADR-0024 D5, 0.0.6)

The optional `experiment` object of `generation.evidence` is what an
Evolution Lab run adds to the record. It is strict: an unknown field, here
or in an artifact, is `invalid_argument`.

| Field | Rule |
| --- | --- |
| `hypothesis` | required; 1..512 bytes, not blank, no control characters |
| `baseline` | optional; an existing Generation other than the candidate (`not_found` if missing) |
| `test_plan` | required; 1..64 distinct check names |
| `metrics` | optional; at most 64 `name: number`, finite |
| `artifacts` | optional; at most 64 `{kind, name, sha256, bytes}` with distinct names |
| `lane` | optional; `reproducibility` or `upstream` |

Check, metric and artifact-kind names are `[a-z0-9._-]`, start with a letter
or digit, and are at most 64 bytes (32 for a kind). An artifact `name` is a
relative path in the run's output directory: 1..128 bytes of
`[A-Za-z0-9._/-]` with no empty, `.` or `..` component. `sha256` is 64
lowercase hex digits.

Without the object the record is the 0.0.5 one (`hypothesis` `generation
acceptance`, empty plan). With it, `hypothesis`, `baseline` and `test_plan`
fill their columns and `metrics` holds `evidence_sha256`, `details_sha256`
(the SHA-256 of the canonical details), `values` and `lane`. Each artifact
becomes an `artifacts` row with the URI `sha256:<hex>` and an
`experiment produced artifact` provenance edge whose note has its `name`
and `bytes`. A retry with the same evidence must carry the same details
(the same `details_sha256`); other or missing details are `conflict` and
write nothing.

`generation.test` sets the Experiment's `decision` to `tested`.
`generation.reject` with `experiment` sets it to `rejected`; the Experiment
must be about that Generation and have a result, otherwise the rejection is
`conflict` (`not_found` if it does not exist) and nothing changes. An
Experiment is decided once: a second decision is `conflict`.

| Command (Admin, read-only) | Required | Optional |
| --- | --- | --- |
| `experiment.inspect` | `id` (Experiment) | |
| `experiment.list` | | `candidate` (Generation), `limit` (1..200, default 50) |

`experiment.inspect` returns the stored record with its `artifacts`;
`experiment.list` returns `count` and `experiments` (newest first, without
metrics and artifacts). `agenticctl generation evidence --experiment-file`
reads the object from a JSON file of at most 1 MiB;
`agenticctl generation reject --experiment` and `agenticctl experiment
inspect|list` map to the commands above.

`generation.activate` and `generation.rollback` validate their arguments
and then refuse with `conflict`, writing nothing, until Gate D is verified.
Only an A/B test image (root-owned test-profile marker) gets further
(ADR-0023 G4b): the slot broker's read-only plan, every database guard on a
rolled-back insert, the backup of an irreversible activation and the
`PENDING` row. The answer carries that row and `arming: true`; it is never
`STABLE` yet. The arm runs after the answer, because the broker hashes both
root slots before it writes the ESP, longer than a request may wait (G5):
read `generation.history` until the row has `armed_at` (then reboot) or is
`ABANDONED` with the broker's reason. A refused plan is `conflict` and
writes nothing; a lost arm answer leaves the row `PENDING` without
`armed_at` for the witness. A retry with the
same `operation_id` returns the recorded row (`already_requested`) and asks
no broker. `acknowledge_irreversible` must equal the broker's verdict: it is
required when the running root could not read the database after the
target's migrations and refused otherwise.

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
