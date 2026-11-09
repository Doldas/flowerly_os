# ADR: Generation lifecycle and durable activation (Gate D)

**Status:** Accepted 2026-11-09 by the owner (direction, decisions Q1–Q6 and
the additional requirements R1–R5 below); to be implemented as G1–G5, one PR
each. **Issues:** #9 (Gate D), #48, #49. **Scope:** 0.0.5. This document
specifies; it does **not** implement activation or rollback in `flowerlyd`.

**No production activation before Gate D is verified.** G2 ships the commands
with activation hard-disabled (`generation.activate` and `generation.rollback`
refuse until an explicit release-gate switch is set); the switch is only
enabled by the change that records the passing G5 evidence.

Builds on [ADR-0019](0019-generation-rootfs-rollback-proposal.md) (accepted:
A/B slots, boot counting, witness; the slot tool `distro-builder slot` exists)
and [ADR-0020](0020-pinned-arch-generation-inputs-proposal.md) (proposed:
pinned inputs). Today's Generation commands change metadata only.

## Verified findings in the current code (2026-11-09, `release/2`)

1. **`activate_generation` has no transition check.** It only requires the row
   to exist and be a `system` Generation. A `REJECTED`, `SUPERSEDED` or
   never-tested `CANDIDATE` system Generation can become `STABLE`.
   (`src/shared/flowerly-core/src/storage/mod.rs`)
2. **No activation history.** Who activated what, when, why and which
   Generation it replaced is not recorded; the only trace is the current
   status column. `promote` is audited (0013), `activate` is not.
3. **Unreachable states.** `TESTED` and `REJECTED` exist in the enum but no
   command reaches them. The docs name `agenticctl generation rollback` and
   `REVOKED`; neither exists. `docs/generations.md` still names `generationd`
   and `crates/agentic-types` (stale).
4. **Public status setter.** `Store::set_generation_status` is `pub`; today its
   only caller is the daemon `self_test`, but nothing prevents a new caller
   from bypassing every invariant.
5. **Nothing links a Generation to a root.** `source_commit` and
   `system_manifest` are free-form strings on `generation.create`; the booted
   root identity (PARTUUID, image SHA-256, kernel/initramfs hashes, build id)
   that the witness records is not part of the record.
6. **`STABLE` is set by a request, not by a boot.** Nothing selects another
   root at the next boot, so `STABLE` can be true while the machine runs
   something else. ADR-0019 forbids exactly this: success must come from the
   witnessed boot.
7. **Rollback across a schema change is unsafe.** Both slots share one SQLite
   database on the data partition. A newer binary migrates it at start-up, i.e.
   *before* the health gate blesses its slot. If that boot then fails and the
   loader returns to the older slot, the older binary refuses the newer schema
   (the #247 guard, correctly) and `flowerlyd` does not start on the
   known-good root. The kernel-level rollback works; the service rollback does
   not.

## Decision

### 1. Lifecycle is typed, enforced in the database, and has no public setter

States: `CANDIDATE`, `TESTED`, `STABLE`, `SUPERSEDED`, `REJECTED`, `REVOKED`.

| From | To | Who / how |
| --- | --- | --- |
| `CANDIDATE` | `TESTED` | `generation.test` — **Admin only** (Q1), records evidence (below) |
| `CANDIDATE`, `TESTED` | `REJECTED` | `generation.reject` — reason, terminal |
| `TESTED` | `STABLE` | only by a **confirmed activation** (§2) |
| `STABLE` | `SUPERSEDED` | in the same transaction as another's confirmation |
| `SUPERSEDED` | `STABLE` | only by a confirmed **rollback** activation to a Generation that was `STABLE` before |
| `TESTED`, `STABLE`, `SUPERSEDED` | `REVOKED` | `generation.revoke` (Admin, reason, audited), terminal |

`REJECTED` and `REVOKED` are terminal. Everything else is refused. The table
lives in the database (`generation_transitions(from_status, to_status)`) and a
`BEFORE UPDATE OF status` trigger aborts any pair not in it, so the invariant
holds for any writer, not just the daemon. Partial unique indexes enforce at
most one `STABLE` system Generation and at most one unresolved activation.
`Store::set_generation_status` becomes crate-private and test-only. Legacy rows
are validated by the migration, not rewritten.

`TESTED` needs verifiable evidence: a provenance edge to an Experiment (or
other result object) the declaring Admin can see, plus the complete typed
root identity of §3. Declaring `TESTED` is audited like `promote`.

### 2. Activation is a durable two-phase record; `STABLE` follows the witness

New table `generation_activations`: `id`, `generation`, `previous_generation`
(the `STABLE` one at request time), `kind` (`ACTIVATE` | `ROLLBACK`), `actor`
(kernel-attested uid, never caller text), `reason`, `operation_id`,
`expected_slot`, `expected_root_partuuid`, `state`, `requested_at`,
`resolved_at`, `witness_boot_id`, `witness_sequence`, `detail`.

`state`: `PENDING` → `CONFIRMED` | `FELL_BACK` | `ABANDONED`.

1. `generation.activate` / `generation.rollback` (Admin) check the §1 table and
   §3 identity, write a `PENDING` row **before** touching anything, then ask the
   broker (§4) to arm the slot. If arming fails or is refused: `ABANDONED` with
   the broker's reason. Nothing is `STABLE` yet.
2. After the reboot, `flowerlyd` resolves every `PENDING` row at start-up from
   the **witness ledger** (read-only, the guest's `boot-witness.jsonl`), never
   from the boot entries alone: the latest witnessed boot's slot and root
   PARTUUID equal the expectation and its sequence is newer than the request →
   `CONFIRMED` and, in one transaction, the Generation becomes `STABLE` and the
   previous one `SUPERSEDED`. The witnessed slot is the previous one →
   `FELL_BACK`; the Generation stays `TESTED`, the row records why, and the same
candidate cannot be activated again without a new explicit Admin decision that
acknowledges that `FELL_BACK` row (reason required, audited).
   Resolution is idempotent and safe to repeat after a crash.
3. Requests carry the existing `operation_id` idempotency key. An unknown
   outcome is never retried blindly (see ipc-mutation-outcomes.md); the client
   reads the activation row.

The slot tool's `status` verdict is **evidence**, not authority: `ACTIVE` and
`FELL_BACK` still require the witness.

### 3. A Generation names its root

`system_manifest` stops being free text. A Generation intended for activation
carries a typed identity: `rootfs_sha256`, `kernel_sha256`,
`initramfs_sha256`, `input_manifest_sha256` (ADR-0020 when it exists),
`build_id`, validated `source_commit` (40 hex). Identity may be incomplete on a
`CANDIDATE` but must be complete to become `TESTED`, and the broker checks it
against the staged slot's hashes before arming. `generation.create` keeps
accepting the optional fields for compatibility; they are validated, not
trusted.

### 4. A small privileged broker arms slots

`flowerlyd` runs as an unprivileged service account and must stay one. A root
**slot broker** (`flowerly-slot-broker`, a socket-activated systemd service
using the `distro-builder slot` code) owns the ESP and slot writes, as ADR-0019
requires. It accepts only typed intents on a root-owned socket restricted by
`SO_PEERCRED` to the `flowerlyd` uid: `arm {slot, tries}` and
`rollback {to}`. It re-verifies everything itself (manifest bound to the GPT,
staged-slot hash, entry sanity) and never accepts a path, a command line or a
Generation id as authority. Staging a new image remains an operator/updater
action (`slot stage`), outside Gate D. Blast radius of a compromised
`flowerlyd`: it can choose between **already staged, hash-verified** slots; it
cannot write arbitrary content or touch the data partition.

### 5. Schema changes must be rollback-compatible

Replace "refuse any newer schema" with an explicit window: each migration
declares `min_reader` (the oldest binary that can still use the database after
it). The database stores the highest `min_reader` it has seen
(`schema_compat`); a binary refuses only if that value is newer than itself.
Migrations are **expand-only** by default (add columns/tables, never drop or
re-type); a contracting migration must raise `min_reader`, and activating a
Generation whose binary raises it past the current `STABLE` binary is an
**irreversible activation**: Admin must confirm it, the pre-activation SQLite
backup (coherent `VACUUM INTO`) is taken and recorded, and automatic fallback
is documented as unavailable for that activation (ADR-0019, "irreversible
migrations block automatic rollback"). The downgrade test is part of the exit.

## Alternatives considered

- **Keep `STABLE` as the thing `activate` writes.** Simple, and wrong: it is the
  false-success ADR-0019 exists to prevent.
- **Let `flowerlyd` write the ESP.** Needs root or a writable ESP for the
  service account; a compromise would be able to alter boot configuration. Rejected.
- **A path-queue (`.path` unit) instead of a socket.** Easier to build, but the
  root side must trust a file an unprivileged process wrote and has no peer
  identity. Kept as a fallback only if the socket proves awkward under systemd.
- **Block any schema bump.** Safe but stops all development; the `min_reader`
  window keeps the check and makes rollback compatibility explicit.

## Work breakdown (one PR each, tests first; R5 applies to every one)

1. **G1 storage:** migration 0015 (transition table + trigger, activation table,
   unique indexes), typed transitions, history API, private setter. Offline
   tests: every legal/illegal pair, direct `UPDATE` refused, one `STABLE`, crash
   at each commit point, legacy rows.
2. **G2 IPC:** `generation.test|reject|revoke|activate|rollback|history`,
   authorization classes, typed args, `agenticctl`. Tests: Admin only, caller
   identity from peer credentials, idempotent retry, redaction.
3. **G3 resolution + schema window (R1, R2, R4):** hash-chained witness and
   health records in the guest, the chain-verifying reader, start-up
   resolution, `min_reader`/`schema_compat`, the baseline-binary compatibility
   tests. Tests: fabricated ledgers for confirm / fell-back / truncated /
   replayed / reordered / chain-broken, crash at every point of resolution,
   old-binary-on-new-schema and the reverse.
4. **G4 broker (R3, R4):** socket service, peercred plus admin-group check,
   self-verification of integrity and schema compatibility, the irreversible
   path (demoted fallback entry, backup); tests with a fake ESP and the real
   tool; shipped only in the test profile at first.
5. **G5 acceptance (real OVMF/KVM, exact SHA):** boot A → `activate` B → reboot
   into B → witness confirms → `STABLE`; `rollback` → A; forced-unhealthy B →
   `FELL_BACK`; downgrade across a schema bump; user data intact; history
   readable after every step. Evidence archived like the S7 runs.

## Owner decisions (2026-11-09)

- **Q1 — `TESTED`:** only an Admin may declare it in 0.0.5, and only with
  verifiable evidence (§1). The Evolution World identity gets no such right yet.
- **Q2 — broker transport:** root-owned Unix socket with `SO_PEERCRED` and
  strict authorization; the broker re-verifies everything itself (R3).
- **Q3 — schema:** `min_reader` plus explicit irreversible activations, but
  backward compatibility must be **proven with the older binary**, and
  automatic fallback must never go to a schema-incompatible root (R4).
- **Q4 — after `FELL_BACK`:** the Generation stays `TESTED`, the reason is
  logged, and re-activating the same candidate needs a new explicit Admin
  decision.
- **Q5 — revoking the active `STABLE`:** refused until another Generation has
  been confirmed `STABLE`.
- **Q6 — `generation.create`:** optional, validated fields stay for backward
  compatibility; a complete typed root identity is required before `TESTED`.

## Additional requirements (owner, 2026-11-09)

**R1 — `STABLE` needs a verified boot identity and a passed health gate.** A
successful activation *request*, an armed entry or a blessed entry is not
enough. Confirmation needs, for one and the same boot: (a) a witness record
whose slot, root PARTUUID, loader entry and baked identity (`source_commit`,
`input_manifest_sha256`, `build_id`) equal the Generation's expectation; and
(b) a **health record** for that boot id, appended by the health gate after the
service-account IPC Ping and before it blesses the entry (the guest gains a
`health` ledger record next to today's `NOT_YET_TESTED` witness). A boot that
was blessed without a matching health record, or has a health record without a
matching witness, confirms nothing.

**R2 — witness resolution is protected against replay, tampering, wrong boot
order and a crash in the middle.**
- *Tamper evidence:* every ledger record carries `prev_sha256` (the SHA-256 of
  the previous line); the reader walks the whole chain and refuses a break, a
  gap or duplicate `sequence`, a duplicate `boot_id`, a malformed or truncated
  record, an oversized file, a symlink, or a file not owned by root / not
  readable only by root and the `flowerly` group.
- *Replay:* the activation row stores the ledger's last `sequence` and head
  hash at request time. Only a record with a strictly higher `sequence`,
  chained from that head, can resolve it. A confirmation is only valid for the
  **newest** record of the chain; resolving the live boot additionally requires
  that record's `boot_id` to equal this kernel's
  `/proc/sys/kernel/random/boot_id`, so a copied or rolled-back ledger cannot
  confirm anything.
- *Boot order:* the boots after the request must read, in order, the expected
  target (counting) and then confirmation; an earlier boot of the *previous*
  slot after a target boot is `FELL_BACK`, not noise.
- *Crash safety:* resolution is one SQLite `IMMEDIATE` transaction (activation
  state, status flip and supersede together) and idempotent; killing the
  process at every point leaves either the old or the fully resolved state, and
  a second run is a no-op.
Each of these has a negative test (fabricated, replayed, reordered, truncated,
chain-broken and crash-injected ledgers).

**R3 — the broker handles only already verified slots and decides for itself.**
It re-checks, on every intent and from its own state, never from the caller:
authorization (the peer uid is the `flowerlyd` service account **and** the
administrator uid that `flowerlyd` attests is still a member of
`flowerly-admin`, read by the broker from the system group database),
integrity (manifest bound to the real GPT, staged-slot hash against the
manifest, the Generation's identity against the baked identity of the staged
image) and schema compatibility (R4). Any mismatch is a refusal with a reason
and no write. It never accepts a path, a command line or a Generation id as
authority, and it refuses everything while activation is disabled.

**R4 — `min_reader` is backed by real compatibility tests, a restore drill and
a technically safe irreversible path.**
- *Tests with the old binary:* the oldest supported reader is built from a
  recorded commit and run against a database migrated by the newest code (read,
  write, IPC Ping, `generation.list`), and the reverse (a newer binary on an
  older database). Every migration must declare `min_reader`; a CI check fails a
  migration without one and without a compatibility test entry. A migration
  that raises `min_reader` past the baseline is *irreversible by definition*.
- *Fallback never goes to an incompatible root:* before arming, the broker
  compares the target's declared post-migration `min_reader` with the
  fallback root's binary version. If the fallback would be incompatible the
  activation is **irreversible**: it needs an explicit Admin acknowledgement,
  and the broker demotes the previous slot's boot entry to manual-only before it
  arms the target, so a failed boot cannot fall back automatically into a root
  whose service cannot read the database; the system then waits in the boot menu
  for an operator.
- *Backup and restore drill:* a coherent pre-activation backup (`VACUUM INTO`,
  integrity-checked, hash recorded on the activation row) is mandatory for an
  irreversible activation, and G5 must restore it and boot the previous slot to
  prove the way back works.

**R5 — every G has negative tests and mandatory quality gates, none weakened.**
Each of G1–G5 ships with refusal/failure-injection tests for what it adds and
passes the required checks unchanged: `cargo fmt`, `clippy -D warnings`,
`cargo test --workspace --locked` (and the slot/broker tests with the real
disk tools), the Ruff gate, `bash scripts/preflight-docs.sh`, the `docs`,
`rust`, CodeQL and security jobs. No test, check or guard may be skipped,
loosened or removed to obtain a green CI; a failing check is fixed at its
cause or reported.

## Non-goals

Package pinning and signatures (ADR-0020/#49), image building, `stage` as an
IPC command, block-device or live-ESP writes by `flowerlyd`, AI-initiated
activation (no preference or model choice grants activation authority),
automatic garbage collection of slots, Secure Boot/dm-verity.
