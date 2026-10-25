-- Explicit object ownership (#225). `created_by` stays the immutable creator
-- audit label; `owner_class`/`owner_uid` are the authoritative owner.
--
--   user        owned by the kernel-attested Linux uid in owner_uid
--   system      shared system object (General, admin-managed records)
--   unresolved  owner unknown: protected, visible to no ordinary user until
--               an audited administrator claim
--
-- New rows default to `unresolved`, so a write path that forgets to record
-- an owner fails closed. Every daemon write path sets it explicitly.
-- The migration runs in one IMMEDIATE transaction with its version record:
-- if any statement fails, nothing here is applied.

ALTER TABLE flows ADD COLUMN owner_class TEXT NOT NULL DEFAULT 'unresolved'
    CHECK (owner_class IN ('user', 'system', 'unresolved'));
ALTER TABLE flows ADD COLUMN owner_uid INTEGER
    CHECK ((owner_class = 'user') = (owner_uid IS NOT NULL)
           AND (owner_uid IS NULL OR owner_uid BETWEEN 0 AND 4294967294));

ALTER TABLE goals ADD COLUMN owner_class TEXT NOT NULL DEFAULT 'unresolved'
    CHECK (owner_class IN ('user', 'system', 'unresolved'));
ALTER TABLE goals ADD COLUMN owner_uid INTEGER
    CHECK ((owner_class = 'user') = (owner_uid IS NOT NULL)
           AND (owner_uid IS NULL OR owner_uid BETWEEN 0 AND 4294967294));

ALTER TABLE sessions ADD COLUMN owner_class TEXT NOT NULL DEFAULT 'unresolved'
    CHECK (owner_class IN ('user', 'system', 'unresolved'));
ALTER TABLE sessions ADD COLUMN owner_uid INTEGER
    CHECK ((owner_class = 'user') = (owner_uid IS NOT NULL)
           AND (owner_uid IS NULL OR owner_uid BETWEEN 0 AND 4294967294));

ALTER TABLE executions ADD COLUMN owner_class TEXT NOT NULL DEFAULT 'unresolved'
    CHECK (owner_class IN ('user', 'system', 'unresolved'));
ALTER TABLE executions ADD COLUMN owner_uid INTEGER
    CHECK ((owner_class = 'user') = (owner_uid IS NOT NULL)
           AND (owner_uid IS NULL OR owner_uid BETWEEN 0 AND 4294967294));

ALTER TABLE generations ADD COLUMN owner_class TEXT NOT NULL DEFAULT 'unresolved'
    CHECK (owner_class IN ('user', 'system', 'unresolved'));
ALTER TABLE generations ADD COLUMN owner_uid INTEGER
    CHECK ((owner_class = 'user') = (owner_uid IS NOT NULL)
           AND (owner_uid IS NULL OR owner_uid BETWEEN 0 AND 4294967294));

ALTER TABLE ai_profiles ADD COLUMN owner_class TEXT NOT NULL DEFAULT 'unresolved'
    CHECK (owner_class IN ('user', 'system', 'unresolved'));
ALTER TABLE ai_profiles ADD COLUMN owner_uid INTEGER
    CHECK ((owner_class = 'user') = (owner_uid IS NOT NULL)
           AND (owner_uid IS NULL OR owner_uid BETWEEN 0 AND 4294967294));

ALTER TABLE ai_providers ADD COLUMN owner_class TEXT NOT NULL DEFAULT 'unresolved'
    CHECK (owner_class IN ('user', 'system', 'unresolved'));
ALTER TABLE ai_providers ADD COLUMN owner_uid INTEGER
    CHECK ((owner_class = 'user') = (owner_uid IS NOT NULL)
           AND (owner_uid IS NULL OR owner_uid BETWEEN 0 AND 4294967294));

ALTER TABLE inference_workloads ADD COLUMN owner_class TEXT NOT NULL DEFAULT 'unresolved'
    CHECK (owner_class IN ('user', 'system', 'unresolved'));
ALTER TABLE inference_workloads ADD COLUMN owner_uid INTEGER
    CHECK ((owner_class = 'user') = (owner_uid IS NOT NULL)
           AND (owner_uid IS NULL OR owner_uid BETWEEN 0 AND 4294967294));

ALTER TABLE inference_usage ADD COLUMN owner_class TEXT NOT NULL DEFAULT 'unresolved'
    CHECK (owner_class IN ('user', 'system', 'unresolved'));
ALTER TABLE inference_usage ADD COLUMN owner_uid INTEGER
    CHECK ((owner_class = 'user') = (owner_uid IS NOT NULL)
           AND (owner_uid IS NULL OR owner_uid BETWEEN 0 AND 4294967294));

-- Backfill. Only a well-formed `uid:<decimal>` creator (written from the
-- kernel peer since the IPC boundary began overwriting created_by) becomes a
-- user owner. Legacy labels such as `user`, empty or malformed values stay
-- unresolved. Nothing is reassigned to any other uid.
UPDATE flows SET owner_class = 'user', owner_uid = CAST(substr(created_by, 5) AS INTEGER)
WHERE created_by GLOB 'uid:[0-9]*' AND substr(created_by, 5) NOT GLOB '*[^0-9]*'
  AND length(created_by) BETWEEN 5 AND 14
  AND CAST(substr(created_by, 5) AS INTEGER) BETWEEN 0 AND 4294967294;
UPDATE goals SET owner_class = 'user', owner_uid = CAST(substr(created_by, 5) AS INTEGER)
WHERE created_by GLOB 'uid:[0-9]*' AND substr(created_by, 5) NOT GLOB '*[^0-9]*'
  AND length(created_by) BETWEEN 5 AND 14
  AND CAST(substr(created_by, 5) AS INTEGER) BETWEEN 0 AND 4294967294;
UPDATE sessions SET owner_class = 'user', owner_uid = CAST(substr(created_by, 5) AS INTEGER)
WHERE created_by GLOB 'uid:[0-9]*' AND substr(created_by, 5) NOT GLOB '*[^0-9]*'
  AND length(created_by) BETWEEN 5 AND 14
  AND CAST(substr(created_by, 5) AS INTEGER) BETWEEN 0 AND 4294967294;
UPDATE ai_profiles SET owner_class = 'user', owner_uid = CAST(substr(created_by, 5) AS INTEGER)
WHERE created_by GLOB 'uid:[0-9]*' AND substr(created_by, 5) NOT GLOB '*[^0-9]*'
  AND length(created_by) BETWEEN 5 AND 14
  AND CAST(substr(created_by, 5) AS INTEGER) BETWEEN 0 AND 4294967294;
UPDATE inference_workloads SET owner_class = 'user', owner_uid = CAST(substr(created_by, 5) AS INTEGER)
WHERE created_by GLOB 'uid:[0-9]*' AND substr(created_by, 5) NOT GLOB '*[^0-9]*'
  AND length(created_by) BETWEEN 5 AND 14
  AND CAST(substr(created_by, 5) AS INTEGER) BETWEEN 0 AND 4294967294;
UPDATE inference_usage SET owner_class = 'user', owner_uid = CAST(substr(created_by, 5) AS INTEGER)
WHERE created_by GLOB 'uid:[0-9]*' AND substr(created_by, 5) NOT GLOB '*[^0-9]*'
  AND length(created_by) BETWEEN 5 AND 14
  AND CAST(substr(created_by, 5) AS INTEGER) BETWEEN 0 AND 4294967294;

-- The protected General Flow (single row, see 0004) is the system Flow.
UPDATE flows SET owner_class = 'system', owner_uid = NULL WHERE name = 'General';

-- Executions carry no creator column: inherit a user owner from their Goal
-- only; otherwise they stay unresolved.
UPDATE executions
SET owner_class = 'user',
    owner_uid = (SELECT goals.owner_uid FROM goals WHERE goals.id = executions.goal_id)
WHERE goal_id IS NOT NULL
  AND (SELECT goals.owner_class FROM goals WHERE goals.id = executions.goal_id) = 'user';

-- Providers are registered only through the Admin-classified
-- provider.register, so they are system definitions.
UPDATE ai_providers SET owner_class = 'system', owner_uid = NULL;

-- Generations record no creator. Stable and superseded Generations are the
-- system image history; candidates, tested and rejected ones have no
-- provable owner and stay unresolved until an audited claim (owner decision:
-- never silently publish existing candidates).
UPDATE generations SET owner_class = 'system', owner_uid = NULL
WHERE status IN ('STABLE', 'SUPERSEDED');

CREATE INDEX IF NOT EXISTS idx_flows_owner ON flows(owner_class, owner_uid);
CREATE INDEX IF NOT EXISTS idx_goals_owner ON goals(owner_class, owner_uid);
CREATE INDEX IF NOT EXISTS idx_sessions_owner ON sessions(owner_class, owner_uid);
CREATE INDEX IF NOT EXISTS idx_executions_owner ON executions(owner_class, owner_uid);
CREATE INDEX IF NOT EXISTS idx_generations_owner ON generations(owner_class, owner_uid);
CREATE INDEX IF NOT EXISTS idx_ai_profiles_owner ON ai_profiles(owner_class, owner_uid);
CREATE INDEX IF NOT EXISTS idx_workloads_owner ON inference_workloads(owner_class, owner_uid);
CREATE INDEX IF NOT EXISTS idx_usage_owner ON inference_usage(owner_class, owner_uid);
