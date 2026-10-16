CREATE TABLE IF NOT EXISTS flows (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    purpose      TEXT,
    parent_flow  TEXT REFERENCES flows(id) ON DELETE SET NULL,
    state        TEXT NOT NULL,
    created_by   TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    last_active  INTEGER,
    revision     INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX IF NOT EXISTS idx_flows_state ON flows(state);
CREATE INDEX IF NOT EXISTS idx_flows_parent ON flows(parent_flow);

CREATE TABLE IF NOT EXISTS projects (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    root_path   TEXT,
    flow_id     TEXT REFERENCES flows(id) ON DELETE SET NULL,
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS goals (
    id              TEXT PRIMARY KEY,
    parent_goal     TEXT REFERENCES goals(id) ON DELETE SET NULL,
    objective       TEXT NOT NULL,
    rationale       TEXT,
    constraints     TEXT NOT NULL DEFAULT '[]',
    success_metrics TEXT NOT NULL DEFAULT '[]',
    priority        INTEGER NOT NULL DEFAULT 0,
    status          TEXT NOT NULL,
    created_by      TEXT NOT NULL,
    created_at      INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_goals_parent ON goals(parent_goal);
CREATE INDEX IF NOT EXISTS idx_goals_status ON goals(status);

CREATE TABLE IF NOT EXISTS agents (
    id          TEXT PRIMARY KEY,
    role        TEXT NOT NULL,
    goal_id     TEXT REFERENCES goals(id) ON DELETE SET NULL,
    status      TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id          TEXT PRIMARY KEY,
    flow_id     TEXT REFERENCES flows(id) ON DELETE SET NULL,
    kind        TEXT NOT NULL,
    pid         INTEGER,
    cwd         TEXT,
    state       TEXT NOT NULL,
    created_by  TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_sessions_flow ON sessions(flow_id);

CREATE TABLE IF NOT EXISTS executions (
    id          TEXT PRIMARY KEY,
    agent_id    TEXT REFERENCES agents(id) ON DELETE SET NULL,
    goal_id     TEXT REFERENCES goals(id) ON DELETE SET NULL,
    reason      TEXT,
    command     TEXT,
    exit_code   INTEGER,
    duration_ms INTEGER,
    started_at  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_executions_goal ON executions(goal_id);

CREATE TABLE IF NOT EXISTS generations (
    id              TEXT PRIMARY KEY,
    parent          TEXT REFERENCES generations(id) ON DELETE SET NULL,
    source_commit   TEXT,
    system_manifest TEXT,
    reason_goal     TEXT REFERENCES goals(id) ON DELETE SET NULL,
    experiment      TEXT,
    status          TEXT NOT NULL,
    created_at      INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS observations (
    id          TEXT PRIMARY KEY,
    kind        TEXT NOT NULL,
    subject     TEXT,
    payload     TEXT NOT NULL DEFAULT '{}',
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS findings (
    id          TEXT PRIMARY KEY,
    producer    TEXT,
    claim       TEXT NOT NULL,
    evidence    TEXT NOT NULL DEFAULT '[]',
    confidence  TEXT,
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS experiments (
    id              TEXT PRIMARY KEY,
    goal_id         TEXT REFERENCES goals(id) ON DELETE SET NULL,
    hypothesis      TEXT NOT NULL,
    candidate       TEXT,
    baseline        TEXT,
    metrics         TEXT NOT NULL DEFAULT '{}',
    test_plan       TEXT NOT NULL DEFAULT '[]',
    result          TEXT,
    decision        TEXT,
    created_at      INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS decisions (
    id          TEXT PRIMARY KEY,
    subject     TEXT,
    outcome     TEXT NOT NULL,
    reasoning   TEXT,
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS artifacts (
    id          TEXT PRIMARY KEY,
    kind        TEXT NOT NULL,
    uri         TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS provenance_edges (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    from_ref    TEXT NOT NULL,
    relation    TEXT NOT NULL,
    to_ref      TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    note        TEXT,
    UNIQUE(from_ref, relation, to_ref)
);

CREATE INDEX IF NOT EXISTS idx_prov_from ON provenance_edges(from_ref);
CREATE INDEX IF NOT EXISTS idx_prov_to ON provenance_edges(to_ref);

CREATE TABLE IF NOT EXISTS user_facts (
    id          TEXT PRIMARY KEY,
    statement   TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS user_observations (
    id          TEXT PRIMARY KEY,
    statement   TEXT NOT NULL,
    source      TEXT,
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS user_inferences (
    id          TEXT PRIMARY KEY,
    statement   TEXT NOT NULL,
    confidence  REAL NOT NULL DEFAULT 0.0,
    invalidated INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL
);
