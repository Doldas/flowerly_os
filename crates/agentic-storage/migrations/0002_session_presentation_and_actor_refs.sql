ALTER TABLE sessions
    ADD COLUMN presentation_state TEXT NOT NULL DEFAULT 'ATTACHED';

UPDATE sessions
SET presentation_state = 'DETACHED'
WHERE state = 'DETACHED';

UPDATE sessions
SET state = 'RUNNING'
WHERE state = 'DETACHED';

UPDATE OR IGNORE provenance_edges
SET to_ref = 'actor:' || to_ref
WHERE relation = 'created_by'
  AND to_ref NOT LIKE 'actor:%';

-- If a normalized duplicate already existed, UPDATE OR IGNORE leaves the
-- legacy row untouched. Remove that now-redundant untyped edge.
DELETE FROM provenance_edges
WHERE relation = 'created_by'
  AND to_ref NOT LIKE 'actor:%';
