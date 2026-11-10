-- The rollback window (ADR-0023 section 5, R4).
--
-- min_reader is the oldest binary that can still use this database, named by
-- the newest migration that binary knows. A binary refuses only a database
-- whose min_reader is newer than itself, so an expand-only migration no
-- longer locks out the previous root. Binaries from before this migration
-- refuse any newer schema, so this migration is irreversible for them.
CREATE TABLE schema_compat (
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    min_reader INTEGER NOT NULL CHECK (min_reader >= 1)
);

INSERT INTO schema_compat (id, min_reader) VALUES (1, 16);

CREATE TRIGGER schema_compat_single_row BEFORE INSERT ON schema_compat
BEGIN SELECT RAISE(ABORT, 'schema_compat has exactly one row'); END;

CREATE TRIGGER schema_compat_only_grows BEFORE UPDATE ON schema_compat
WHEN NEW.min_reader < OLD.min_reader OR NEW.id IS NOT OLD.id
BEGIN SELECT RAISE(ABORT, 'the reader window only grows'); END;

CREATE TRIGGER schema_compat_no_delete BEFORE DELETE ON schema_compat
BEGIN SELECT RAISE(ABORT, 'the reader window cannot be removed'); END;
