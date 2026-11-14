# Flowerly OS — Generations

A generation is a reproducible, reversible snapshot of system state. System
changes move through candidate generations, are tested in the Evolution World,
and are promoted or rejected based on evidence. User data is kept separate from
system-generation rollback: rolling back the system never discards user data.

## Data model

Defined in `src/shared/flowerly-core` (`Generation`) and persisted by
`flowerlyd` in the shared SQLite database:

```text
Generation {
    id:              GenerationId // gen-<uuid>
    parent:          Option<GenerationId>
    source_commit:   Option<String>  // git commit the candidate was built from
    system_manifest: Option<String>
    reason_goal:     Option<GoalId>  // the goal that motivated the change
    experiment:      Option<ExperimentId>
    status:          GenerationStatus
    created_at:      Timestamp
    owner:           Owner
}
```

A Generation meant for activation also has a typed root identity: the
SHA-256 of its root filesystem, kernel, initramfs and input manifest, and its
build id.

## Status lifecycle

The states and their rules are specified by
[ADR-0023](adr/0023-generation-lifecycle-and-durable-activation.md) and
enforced by the database for every writer:

- `CANDIDATE` — built or being built and not yet trusted.
- `TESTED` — an Admin declared it tested, from recorded passing evidence and a
  complete root identity.
- `STABLE` — confirmed active on the machine. Only a confirmed activation can
  set it, and at most one Generation holds it.
- `SUPERSEDED` — a newer stable generation replaced it.
- `REJECTED` — an explicit decision against it (terminal).
- `REVOKED` — withdrawn because a defect or risk was discovered (terminal).

## Arch package generations

Arch Linux is the initial userspace/package substrate. Its rolling release model
does not mean Stable World rolls continuously.

An Arch repository update is an input to generation construction:

```text
upstream package delta
        ↓
resolve a full candidate package set
        ↓
record exact package/kernel/build identities
        ↓
build and test candidate
        ↓
promote or reject
```

The package-name selection under `image/arch/` is not itself a generation
manifest. A promoted generation must eventually record exact versions,
architecture, package hashes/repository acquisition context, kernel identity,
Flowerly source/output hashes, and its parent/provenance. Flowerly may retain
the exact package artifacts or a repository snapshot needed to reproduce
promoted generations.

This also means rollback is not implemented by attempting to reverse a sequence
of pacman transactions. Rollback selects a known previous Flowerly generation.

## Activation and rollback

**Not available yet.** `agenticctl generation activate` and `rollback` refuse
until Gate D is verified. `generation list|create|inspect|promote|identity|evidence|test|reject|revoke|history`
work on the records only; none of them selects a root. On the A/B test image
only, `activate` and `rollback` (with `--reason`) record a `PENDING` request
and ask the root slot broker to arm the inactive slot; an activation the
running root could not fall back from needs `--acknowledge-irreversible`,
takes a database backup first and leaves the old root in the boot menu only.

When Gate D is complete, an activation is a durable two-phase record: an Admin
requests it, the slot is armed, and `STABLE` follows only from the witnessed
boot and its health gate. Generation history survives reboot, and user data is
kept separate from system rollback.

## Implemented status and planned extensions

- **Implemented:** the `Generation` model, typed transitions enforced in the
  database, activation history storage, root identity, recorded evidence and
  provenance links.
- **Planned (G3–G5 of ADR-0023):** witness-based confirmation, the
  `min_reader` schema window, the slot broker and the real KVM acceptance.
