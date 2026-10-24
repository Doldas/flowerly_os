# Storage mode contract (proposed, #85)

Flowerly currently stores all production state in its local `state.db`.
This phase introduces a typed storage-mode **contract**, not new storage
backends or a switchable production database layout.

`StorageMode` has exactly three values: `standalone` (default),
`cluster` and `core_only`. The future system installation location is
`/etc/flowerly/mode`; absent mode files mean `standalone`, preserving
compatibility with existing installations. A present file must contain one
canonical lowercase value and may end with one newline. Invalid UTF-8,
unexpected values, additional lines and file read errors are rejected.

**Safety constraints:** Neither cluster replication nor remote listeners
are enabled by these values today. A mode transition to a different value
returns `UnsupportedTransition` until reversible storage migrations and
the approval policy exist. In particular, selecting `core_only` must
never delete memory data, and a prospective cluster must never replicate
core system state.

The contract is currently a library API; it is **not automatically read
by running daemons** and does not change `AGENTIC_DATABASE_PATH`,
startup, shutdown or the current single SQLite database. Wiring it into
installed runtimes requires explicit migration/recovery tests and an ADR
decision under #82–#85, as well as offline/restart/resource-profile
acceptance before marking #85 complete.

Run `cargo test -p flowerly-core storage_mode` to exercise the
negative/positive parsing and transition contract. Real installation and
no-open-port integration tests are separate follow-ups.
