-- Test images only (feature g5-schema-drill, ADR-0023 G5). A binary that
-- knows migration 16 at most must not use this database any more: the
-- migration declares min_reader 17, which makes activating it irreversible
-- and going back possible only by restoring the pre-activation backup.
CREATE TABLE g5_schema_drill (
    id   INTEGER PRIMARY KEY CHECK (id = 1),
    note TEXT NOT NULL
);

INSERT INTO g5_schema_drill (id, note)
VALUES (1, 'written by a schema-17 test image; unreadable for schema 16');
