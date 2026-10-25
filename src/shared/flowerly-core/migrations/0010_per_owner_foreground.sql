-- Per-owner foreground Flow and same-owner references (#225).
--
-- The foreground is no longer a lifecycle state of a shared Flow row. Each
-- kernel-attested uid has at most one row in `flow_foreground` (its primary
-- key). No row means that uid's foreground is the canonical General Flow, so
-- activating General deletes only the caller's row and never changes anyone
-- else's state. Only an unarchived Flow owned by the same uid can be stored:
-- General, other system Flows, unresolved and other users' Flows cannot.
--
-- The whole file runs in one IMMEDIATE transaction with its version record
-- (Store::migrate): a failure anywhere leaves 0009 state untouched, including
-- idx_flows_single_foreground.

CREATE TABLE flow_foreground (
    owner_uid    INTEGER PRIMARY KEY
                 CHECK (owner_uid BETWEEN 0 AND 4294967294),
    flow_id      TEXT NOT NULL REFERENCES flows(id) ON DELETE CASCADE,
    activated_at INTEGER NOT NULL
);

CREATE INDEX idx_flow_foreground_flow ON flow_foreground(flow_id);

CREATE TRIGGER flow_foreground_insert_owned
BEFORE INSERT ON flow_foreground
WHEN NOT EXISTS (
    SELECT 1 FROM flows
    WHERE id = NEW.flow_id AND owner_class = 'user'
      AND owner_uid = NEW.owner_uid AND state != 'ARCHIVED'
)
BEGIN
    SELECT RAISE(ABORT, 'foreground Flow must be an unarchived Flow of the same owner');
END;

CREATE TRIGGER flow_foreground_update_owned
BEFORE UPDATE ON flow_foreground
WHEN NOT EXISTS (
    SELECT 1 FROM flows
    WHERE id = NEW.flow_id AND owner_class = 'user'
      AND owner_uid = NEW.owner_uid AND state != 'ARCHIVED'
)
BEGIN
    SELECT RAISE(ABORT, 'foreground Flow must be an unarchived Flow of the same owner');
END;

-- A Flow that is archived or changes owner stops being anyone's foreground
-- in the same statement; that owner falls back to General. No dangling row
-- can survive archive, ownership transfer or delete (ON DELETE CASCADE).
CREATE TRIGGER flow_foreground_follows_flow
AFTER UPDATE OF state, owner_class, owner_uid ON flows
BEGIN
    DELETE FROM flow_foreground
    WHERE flow_id = NEW.id
      AND NOT (NEW.owner_class = 'user' AND NEW.owner_uid = flow_foreground.owner_uid
               AND NEW.state != 'ARCHIVED');
END;

-- Carry the legacy single foreground over to its owner when that owner is a
-- resolved user; General, system and unresolved foregrounds become the
-- General fallback for everyone. Nothing is assigned to any other uid.
INSERT INTO flow_foreground (owner_uid, flow_id, activated_at)
SELECT owner_uid, id, COALESCE(last_active, created_at)
FROM flows
WHERE state = 'FOREGROUND' AND owner_class = 'user';

UPDATE flows SET state = 'ACTIVE', revision = revision + 1 WHERE state = 'FOREGROUND';

DROP INDEX idx_flows_single_foreground;

-- FOREGROUND is no longer a lifecycle state.
CREATE TRIGGER flows_reject_foreground_insert
BEFORE INSERT ON flows
WHEN NEW.state = 'FOREGROUND'
BEGIN
    SELECT RAISE(ABORT, 'FOREGROUND is per owner (flow_foreground), not a Flow state');
END;

CREATE TRIGGER flows_reject_foreground_update
BEFORE UPDATE OF state ON flows
WHEN NEW.state = 'FOREGROUND'
BEGIN
    SELECT RAISE(ABORT, 'FOREGROUND is per owner (flow_foreground), not a Flow state');
END;

-- Same-owner references for new rows. A Goal's parent and an Execution's
-- Goal must have the Goal's/Execution's owner; a Session's Flow must have
-- the Session's owner or be the canonical General. The daemon checks these
-- in the same transaction and answers `not_found`; these triggers are the
-- database boundary for any other writer. Legacy rows are not rewritten.
CREATE TRIGGER goals_parent_same_owner
BEFORE INSERT ON goals
WHEN NEW.parent_goal IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM goals AS parent
    WHERE parent.id = NEW.parent_goal AND parent.owner_class = NEW.owner_class
      AND parent.owner_uid IS NEW.owner_uid
)
BEGIN
    SELECT RAISE(ABORT, 'parent Goal must have the same owner');
END;

CREATE TRIGGER executions_goal_same_owner
BEFORE INSERT ON executions
WHEN NEW.goal_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM goals
    WHERE goals.id = NEW.goal_id AND goals.owner_class = NEW.owner_class
      AND goals.owner_uid IS NEW.owner_uid
)
BEGIN
    SELECT RAISE(ABORT, 'execution Goal must have the same owner');
END;

CREATE TRIGGER sessions_flow_same_owner
BEFORE INSERT ON sessions
WHEN NEW.flow_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM flows
    WHERE flows.id = NEW.flow_id AND (
        (flows.owner_class = NEW.owner_class AND flows.owner_uid IS NEW.owner_uid)
        OR (flows.name = 'General' AND flows.owner_class = 'system')
    )
)
BEGIN
    SELECT RAISE(ABORT, 'session Flow must have the same owner or be General');
END;
