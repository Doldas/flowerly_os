-- Private Generation candidates and the audited Admin ownership log (#225
-- PR 6). Existing rows are not rewritten: legacy candidates stay
-- `unresolved` until an Admin promotes or claims them explicitly.

-- A Generation may name only a parent its owner can see: its own or a
-- system one. generation.create checks this in the inserting transaction
-- and answers `not_found`; this trigger is the boundary for any other writer.
CREATE TRIGGER generations_parent_visible
BEFORE INSERT ON generations
WHEN NEW.parent IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM generations AS p
    WHERE p.id = NEW.parent AND (
        p.owner_class = 'system'
        OR (p.owner_class = NEW.owner_class AND p.owner_uid IS NEW.owner_uid))
)
BEGIN
    SELECT RAISE(ABORT, 'parent Generation must be the owner''s own or a system Generation');
END;

-- Append-only record of every applied Admin operation that crosses or
-- changes ownership. Written in the same transaction as the change it
-- records; refusals are journaled (`flowerly::admin_audit`), not stored.
CREATE TABLE admin_audit (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at      INTEGER NOT NULL,
    actor_uid       INTEGER NOT NULL CHECK (actor_uid BETWEEN 0 AND 4294967294),
    command         TEXT NOT NULL,
    object_ref      TEXT NOT NULL,
    from_owner_class TEXT NOT NULL CHECK (from_owner_class IN ('user', 'system', 'unresolved')),
    from_owner_uid  INTEGER,
    to_owner_class  TEXT NOT NULL CHECK (to_owner_class IN ('user', 'system', 'unresolved')),
    to_owner_uid    INTEGER,
    reason          TEXT NOT NULL CHECK (length(reason) BETWEEN 1 AND 512),
    outcome         TEXT NOT NULL CHECK (outcome = 'applied')
);

CREATE INDEX idx_admin_audit_object ON admin_audit(object_ref, id);

CREATE TRIGGER admin_audit_no_update
BEFORE UPDATE ON admin_audit
BEGIN
    SELECT RAISE(ABORT, 'admin_audit is append-only');
END;

CREATE TRIGGER admin_audit_no_delete
BEFORE DELETE ON admin_audit
BEGIN
    SELECT RAISE(ABORT, 'admin_audit is append-only');
END;
