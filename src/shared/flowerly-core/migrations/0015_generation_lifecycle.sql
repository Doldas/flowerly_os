-- Generation lifecycle and durable activation (ADR-0023, G1).
--
-- The invariants live in the database so that they hold for every writer, not
-- only for the daemon: typed status transitions, at most one STABLE system
-- Generation, a STABLE that exists only through a CONFIRMED activation, an
-- append-only activation history, and a complete root identity before TESTED.
-- Nothing here selects a root or activates anything: confirmation is written by
-- the witness resolution of G3, and production activation stays disabled until
-- Gate D is verified.

-- Legacy rows are validated, not rewritten: a database that already holds two
-- STABLE rows fails this index and the whole migration, leaving it unchanged.
CREATE UNIQUE INDEX generations_one_stable ON generations(status) WHERE status = 'STABLE';

-- Legal transitions. STABLE -> REVOKED is deliberately absent: the active
-- Generation cannot be revoked until another one is confirmed STABLE (and it is
-- then SUPERSEDED, which can be revoked).
CREATE TABLE generation_transitions (
    from_status TEXT NOT NULL,
    to_status   TEXT NOT NULL,
    PRIMARY KEY (from_status, to_status)
) WITHOUT ROWID;

INSERT INTO generation_transitions (from_status, to_status) VALUES
    ('CANDIDATE',  'TESTED'),
    ('CANDIDATE',  'REJECTED'),
    ('TESTED',     'REJECTED'),
    ('TESTED',     'STABLE'),
    ('STABLE',     'SUPERSEDED'),
    ('SUPERSEDED', 'STABLE'),
    ('TESTED',     'REVOKED'),
    ('SUPERSEDED', 'REVOKED');

CREATE TRIGGER generation_transitions_no_insert BEFORE INSERT ON generation_transitions
BEGIN SELECT RAISE(ABORT, 'the Generation transition table is fixed'); END;
CREATE TRIGGER generation_transitions_no_update BEFORE UPDATE ON generation_transitions
BEGIN SELECT RAISE(ABORT, 'the Generation transition table is fixed'); END;
CREATE TRIGGER generation_transitions_no_delete BEFORE DELETE ON generation_transitions
BEGIN SELECT RAISE(ABORT, 'the Generation transition table is fixed'); END;

CREATE TRIGGER generations_start_as_candidate BEFORE INSERT ON generations
WHEN NEW.status <> 'CANDIDATE'
BEGIN SELECT RAISE(ABORT, 'a new Generation starts as CANDIDATE'); END;

-- INSERT OR REPLACE would delete the row without firing a delete trigger.
CREATE TRIGGER generations_no_replace BEFORE INSERT ON generations
WHEN EXISTS (SELECT 1 FROM generations WHERE id = NEW.id)
BEGIN SELECT RAISE(ABORT, 'a Generation with this id already exists'); END;

CREATE TRIGGER generations_id_immutable BEFORE UPDATE OF id ON generations
WHEN NEW.id IS NOT OLD.id
BEGIN SELECT RAISE(ABORT, 'a Generation id is immutable'); END;

CREATE TRIGGER generations_no_delete BEFORE DELETE ON generations
WHEN OLD.status <> 'CANDIDATE'
BEGIN SELECT RAISE(ABORT, 'a Generation that left CANDIDATE cannot be deleted'); END;

CREATE TRIGGER generations_status_transition BEFORE UPDATE OF status ON generations
WHEN NEW.status IS NOT OLD.status AND NOT EXISTS (
    SELECT 1 FROM generation_transitions
    WHERE from_status = OLD.status AND to_status = NEW.status)
BEGIN SELECT RAISE(ABORT, 'illegal Generation status transition'); END;

-- The target of an unresolved activation must stay resolvable.
CREATE TRIGGER generations_no_finish_while_pending BEFORE UPDATE OF status ON generations
WHEN NEW.status IN ('REJECTED', 'REVOKED') AND EXISTS (
    SELECT 1 FROM generation_activations a
    WHERE a.generation = NEW.id AND a.state = 'PENDING')
BEGIN SELECT RAISE(ABORT, 'the Generation is the target of a PENDING activation'); END;

-- Complete typed root identity (what a witnessed boot can be compared with).
CREATE TABLE generation_identities (
    generation            TEXT PRIMARY KEY REFERENCES generations(id) ON DELETE RESTRICT,
    rootfs_sha256         TEXT CHECK (rootfs_sha256 IS NULL OR (length(rootfs_sha256) = 64
                                 AND rootfs_sha256 NOT GLOB '*[^0-9a-f]*')),
    kernel_sha256         TEXT CHECK (kernel_sha256 IS NULL OR (length(kernel_sha256) = 64
                                 AND kernel_sha256 NOT GLOB '*[^0-9a-f]*')),
    initramfs_sha256      TEXT CHECK (initramfs_sha256 IS NULL OR (length(initramfs_sha256) = 64
                                 AND initramfs_sha256 NOT GLOB '*[^0-9a-f]*')),
    input_manifest_sha256 TEXT CHECK (input_manifest_sha256 IS NULL OR (length(input_manifest_sha256) = 64
                                 AND input_manifest_sha256 NOT GLOB '*[^0-9a-f]*')),
    build_id              TEXT CHECK (build_id IS NULL OR (length(build_id) BETWEEN 1 AND 64
                                 AND build_id NOT GLOB '*[^A-Za-z0-9_.-]*'))
);

CREATE TRIGGER generation_identities_insert_candidate_only BEFORE INSERT ON generation_identities
WHEN (SELECT status FROM generations WHERE id = NEW.generation) IS NOT 'CANDIDATE'
BEGIN SELECT RAISE(ABORT, 'a root identity can only be set while the Generation is a CANDIDATE'); END;
CREATE TRIGGER generation_identities_update_candidate_only BEFORE UPDATE ON generation_identities
WHEN (SELECT status FROM generations WHERE id = OLD.generation) IS NOT 'CANDIDATE'
  OR NEW.generation IS NOT OLD.generation
BEGIN SELECT RAISE(ABORT, 'a root identity can only change while the Generation is a CANDIDATE'); END;
CREATE TRIGGER generation_identities_no_delete BEFORE DELETE ON generation_identities
BEGIN SELECT RAISE(ABORT, 'a root identity is never deleted'); END;

-- The commit and manifest text freeze together with the identity.
CREATE TRIGGER generations_identity_frozen BEFORE UPDATE OF source_commit, system_manifest ON generations
WHEN OLD.status <> 'CANDIDATE'
  AND (NEW.source_commit IS NOT OLD.source_commit OR NEW.system_manifest IS NOT OLD.system_manifest)
BEGIN SELECT RAISE(ABORT, 'source commit and manifest are frozen once a Generation leaves CANDIDATE'); END;

-- TESTED and STABLE need the complete identity and a real source commit.
CREATE TRIGGER generations_identity_complete BEFORE UPDATE OF status ON generations
WHEN NEW.status IN ('TESTED', 'STABLE') AND NEW.status IS NOT OLD.status AND NOT (
    NEW.source_commit IS NOT NULL
    AND length(NEW.source_commit) = 40
    AND NEW.source_commit NOT GLOB '*[^0-9a-f]*'
    AND EXISTS (
        SELECT 1 FROM generation_identities i
        WHERE i.generation = NEW.id
          AND i.rootfs_sha256 IS NOT NULL AND i.kernel_sha256 IS NOT NULL
          AND i.initramfs_sha256 IS NOT NULL AND i.input_manifest_sha256 IS NOT NULL
          AND i.build_id IS NOT NULL))
BEGIN SELECT RAISE(ABORT, 'Generation root identity is incomplete'); END;

-- STABLE is never set by a request: only the newest CONFIRMED activation of
-- exactly this Generation, with the matching kind, can make it STABLE.
CREATE TRIGGER generations_stable_needs_confirmation BEFORE UPDATE OF status ON generations
WHEN NEW.status = 'STABLE' AND OLD.status IS NOT 'STABLE' AND NOT EXISTS (
    SELECT 1 FROM generation_activations a
    WHERE a.id = (SELECT MAX(id) FROM generation_activations WHERE state = 'CONFIRMED')
      AND a.generation = NEW.id
      AND a.kind = CASE OLD.status WHEN 'TESTED' THEN 'ACTIVATE' WHEN 'SUPERSEDED' THEN 'ROLLBACK' END)
BEGIN SELECT RAISE(ABORT, 'a Generation becomes STABLE only through a confirmed activation'); END;

-- The previous STABLE is superseded only as the consequence of that same
-- confirmation, never on its own.
CREATE TRIGGER generations_supersede_needs_confirmation BEFORE UPDATE OF status ON generations
WHEN OLD.status = 'STABLE' AND NEW.status = 'SUPERSEDED' AND NOT EXISTS (
    SELECT 1 FROM generation_activations a
    WHERE a.id = (SELECT MAX(id) FROM generation_activations WHERE state = 'CONFIRMED')
      AND a.previous_generation = OLD.id)
BEGIN SELECT RAISE(ABORT, 'a STABLE Generation is superseded only by a confirmed activation'); END;

-- Append-only activation history (ADR-0023 section 2).
CREATE TABLE generation_activations (
    id                     INTEGER PRIMARY KEY AUTOINCREMENT,
    generation             TEXT NOT NULL REFERENCES generations(id) ON DELETE RESTRICT,
    previous_generation    TEXT REFERENCES generations(id) ON DELETE RESTRICT,
    kind                   TEXT NOT NULL CHECK (kind IN ('ACTIVATE', 'ROLLBACK')),
    actor_uid              INTEGER NOT NULL CHECK (actor_uid BETWEEN 0 AND 4294967294),
    reason                 TEXT NOT NULL CHECK (length(reason) BETWEEN 1 AND 512),
    operation_id           TEXT CHECK (operation_id IS NULL OR length(operation_id) BETWEEN 1 AND 128),
    request_digest         TEXT NOT NULL CHECK (length(request_digest) = 64
                                  AND request_digest NOT GLOB '*[^0-9a-f]*'),
    expected_slot          TEXT NOT NULL CHECK (expected_slot IN ('root-a', 'root-b')),
    expected_root_partuuid TEXT NOT NULL CHECK (length(expected_root_partuuid) = 36
                                  AND expected_root_partuuid NOT GLOB '*[^0-9a-f-]*'),
    ledger_sequence        INTEGER NOT NULL CHECK (ledger_sequence >= 0),
    ledger_head            TEXT NOT NULL CHECK (length(ledger_head) = 64
                                  AND ledger_head NOT GLOB '*[^0-9a-f]*'),
    irreversible           INTEGER NOT NULL DEFAULT 0 CHECK (irreversible IN (0, 1)),
    backup_sha256          TEXT CHECK (backup_sha256 IS NULL OR (length(backup_sha256) = 64
                                  AND backup_sha256 NOT GLOB '*[^0-9a-f]*')),
    acknowledges           INTEGER REFERENCES generation_activations(id) ON DELETE RESTRICT,
    state                  TEXT NOT NULL DEFAULT 'PENDING'
                           CHECK (state IN ('PENDING', 'CONFIRMED', 'FELL_BACK', 'ABANDONED')),
    requested_at           INTEGER NOT NULL,
    resolved_at            INTEGER,
    witness_boot_id        TEXT,
    witness_sequence       INTEGER,
    detail                 TEXT CHECK (detail IS NULL OR length(detail) <= 1024),
    CHECK ((state = 'PENDING') = (resolved_at IS NULL)),
    -- Replay guard: a confirmation needs a witnessed record strictly newer
    -- than the ledger position the request was made at.
    CHECK (state <> 'CONFIRMED' OR (witness_boot_id IS NOT NULL
                                    AND witness_sequence IS NOT NULL
                                    AND witness_sequence > ledger_sequence)),
    -- An irreversible activation must carry its pre-activation backup hash.
    CHECK (irreversible = 0 OR backup_sha256 IS NOT NULL)
);

-- At most one unresolved activation and at most one per operation id.
CREATE UNIQUE INDEX generation_activations_one_pending ON generation_activations(state)
    WHERE state = 'PENDING';
CREATE UNIQUE INDEX generation_activations_operation ON generation_activations(operation_id)
    WHERE operation_id IS NOT NULL;
CREATE INDEX generation_activations_generation ON generation_activations(generation, id);

CREATE TRIGGER generation_activations_request_checks BEFORE INSERT ON generation_activations
BEGIN
    SELECT RAISE(ABORT, 'a new activation starts PENDING') WHERE NEW.state <> 'PENDING';
    SELECT RAISE(ABORT, 'only a system Generation can be activated')
        WHERE (SELECT owner_class FROM generations WHERE id = NEW.generation) IS NOT 'system';
    SELECT RAISE(ABORT, 'activation needs a TESTED Generation, rollback a SUPERSEDED one')
        WHERE (SELECT status FROM generations WHERE id = NEW.generation)
              IS NOT CASE NEW.kind WHEN 'ACTIVATE' THEN 'TESTED' ELSE 'SUPERSEDED' END;
    SELECT RAISE(ABORT, 'the previous Generation must be the current STABLE one')
        WHERE NEW.previous_generation IS NOT (SELECT id FROM generations WHERE status = 'STABLE');
    SELECT RAISE(ABORT, 'rollback needs a Generation that was confirmed STABLE before')
        WHERE NEW.kind = 'ROLLBACK' AND (NEW.previous_generation IS NULL OR NOT EXISTS (
            SELECT 1 FROM generation_activations
            WHERE generation = NEW.generation AND state = 'CONFIRMED'));
    SELECT RAISE(ABORT, 'Generation root identity is incomplete')
        WHERE NOT (
            (SELECT length(source_commit) FROM generations WHERE id = NEW.generation) = 40
            AND EXISTS (
                SELECT 1 FROM generation_identities i
                WHERE i.generation = NEW.generation
                  AND i.rootfs_sha256 IS NOT NULL AND i.kernel_sha256 IS NOT NULL
                  AND i.initramfs_sha256 IS NOT NULL AND i.input_manifest_sha256 IS NOT NULL
                  AND i.build_id IS NOT NULL));
    SELECT RAISE(ABORT, 'a FELL_BACK activation must be acknowledged by the new request')
        WHERE NEW.kind = 'ACTIVATE' AND EXISTS (
            SELECT 1 FROM generation_activations f
            WHERE f.generation = NEW.generation AND f.state = 'FELL_BACK'
              AND f.id IS NOT NEW.acknowledges
              AND NOT EXISTS (SELECT 1 FROM generation_activations a
                              WHERE a.acknowledges = f.id AND a.state <> 'ABANDONED'));
    SELECT RAISE(ABORT, 'acknowledges must name a FELL_BACK activation of this Generation')
        WHERE NEW.acknowledges IS NOT NULL AND NOT EXISTS (
            SELECT 1 FROM generation_activations f
            WHERE f.id = NEW.acknowledges AND f.generation = NEW.generation
              AND f.state = 'FELL_BACK');
END;

CREATE TRIGGER generation_activations_resolve_only BEFORE UPDATE ON generation_activations
BEGIN
    SELECT RAISE(ABORT, 'a resolved activation is immutable') WHERE OLD.state <> 'PENDING';
    SELECT RAISE(ABORT, 'an activation resolves to CONFIRMED, FELL_BACK or ABANDONED')
        WHERE NEW.state = 'PENDING';
    SELECT RAISE(ABORT, 'only the resolution columns of an activation can change')
        WHERE NEW.id IS NOT OLD.id OR NEW.generation IS NOT OLD.generation
           OR NEW.previous_generation IS NOT OLD.previous_generation
           OR NEW.kind IS NOT OLD.kind OR NEW.actor_uid IS NOT OLD.actor_uid
           OR NEW.reason IS NOT OLD.reason OR NEW.operation_id IS NOT OLD.operation_id
           OR NEW.request_digest IS NOT OLD.request_digest
           OR NEW.expected_slot IS NOT OLD.expected_slot
           OR NEW.expected_root_partuuid IS NOT OLD.expected_root_partuuid
           OR NEW.ledger_sequence IS NOT OLD.ledger_sequence
           OR NEW.ledger_head IS NOT OLD.ledger_head
           OR NEW.irreversible IS NOT OLD.irreversible
           OR NEW.backup_sha256 IS NOT OLD.backup_sha256
           OR NEW.acknowledges IS NOT OLD.acknowledges
           OR NEW.requested_at IS NOT OLD.requested_at;
END;

CREATE TRIGGER generation_activations_no_delete BEFORE DELETE ON generation_activations
BEGIN SELECT RAISE(ABORT, 'activation history is append-only'); END;
