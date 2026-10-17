-- Local AI workload admission and usage accounting (RFC 0004 tranche 3).
-- Admission rows record decisions with their own reasons and the resolved
-- ceiling snapshot from the profile at submit time. Usage rows are the
-- token/latency/cost ledger the later provider boundary reports against.
-- No execution, no process control and no network live here.
CREATE TABLE IF NOT EXISTS inference_workloads (
    id             TEXT PRIMARY KEY,
    profile_id     TEXT NOT NULL,
    profile_name   TEXT NOT NULL,
    model_id       TEXT NOT NULL,
    background     TEXT NOT NULL,
    priority       INTEGER NOT NULL,
    state          TEXT NOT NULL,
    reason         TEXT NOT NULL,
    ceilings       TEXT NOT NULL,
    created_by     TEXT NOT NULL,
    created_at     INTEGER NOT NULL,
    admitted_at    INTEGER,
    stopped_at     INTEGER,
    completed_at   INTEGER,
    operation_id   TEXT
);

CREATE INDEX IF NOT EXISTS idx_inference_workloads_state
    ON inference_workloads(state);
CREATE INDEX IF NOT EXISTS idx_inference_workloads_profile
    ON inference_workloads(profile_id, state);

CREATE TABLE IF NOT EXISTS inference_usage (
    id          TEXT PRIMARY KEY,
    workload_id TEXT NOT NULL,
    model_id    TEXT NOT NULL,
    tokens_in   INTEGER NOT NULL,
    tokens_out  INTEGER NOT NULL,
    latency_ms  INTEGER NOT NULL,
    cost_micros INTEGER NOT NULL,
    created_by  TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_inference_usage_model
    ON inference_usage(model_id);
CREATE INDEX IF NOT EXISTS idx_inference_usage_time
    ON inference_usage(created_at);
