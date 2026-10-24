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

Inspect/activate require `id` to be a valid Generation ID. Unknown
object keys continue to be ignored to preserve forwards compatibility
with existing IPC v1 clients. Unknown command names still return
`invalid_argument`; missing referenced objects return `not_found`.
SQLite busy remains `unavailable`, known unique conflicts
`conflict`, and corruption/other SQLite failures `internal`.

No IPC envelope or protocol version has changed. This is an
incremental Generation command contract only; remaining commands and
domain services still need the full #53 audit and negative tests.
