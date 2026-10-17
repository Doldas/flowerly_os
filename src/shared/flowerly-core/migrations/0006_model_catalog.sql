-- Local model catalog (RFC 0004 prototype tranche).
-- Durable, measured-metadata-only model registry rows. No provider secrecy,
-- no credentials, no remote endpoints and no OS authorization lives here.
-- Memory/context values are recorded measurements with an evidence label;
-- NULL means unknown and must be reported as unknown, never estimated.
CREATE TABLE IF NOT EXISTS models (
    id                    TEXT PRIMARY KEY,
    name                  TEXT NOT NULL,
    family                TEXT,
    architecture          TEXT,
    backend               TEXT NOT NULL,
    context_tokens        INTEGER,
    measured_memory_bytes INTEGER,
    modalities            TEXT NOT NULL,
    license               TEXT,
    evidence              TEXT,
    created_by            TEXT NOT NULL,
    created_at            INTEGER NOT NULL,
    retired_at            INTEGER
);

CREATE INDEX IF NOT EXISTS idx_models_family ON models(family);
CREATE INDEX IF NOT EXISTS idx_models_name ON models(name);
