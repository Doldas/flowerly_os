ALTER TABLE sessions
    ADD COLUMN presentation_state TEXT NOT NULL DEFAULT 'ATTACHED';

UPDATE sessions
SET presentation_state = 'DETACHED'
WHERE state = 'DETACHED';

UPDATE sessions
SET state = 'RUNNING'
WHERE state = 'DETACHED';

UPDATE provenance_edges
SET to_ref = 'actor:' || to_ref
WHERE relation = 'created_by'
  AND to_ref NOT LIKE 'actor:%';
