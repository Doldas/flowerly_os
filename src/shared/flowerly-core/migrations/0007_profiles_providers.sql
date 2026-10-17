-- Expert AI profiles and inference provider registry (RFC 0004 tranche 2).
-- Profiles are portable, credential-free preference documents, versioned
-- append-only so every edit keeps the prior version auditable. Providers register
-- capabilities only; remote providers are structurally rejected until egress
-- policy authorization exists. Nothing here grants OS authority.
CREATE TABLE IF NOT EXISTS ai_profiles (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    created_by  TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_ai_profiles_name ON ai_profiles(name);

CREATE TABLE IF NOT EXISTS ai_profile_versions (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    profile_id  TEXT NOT NULL REFERENCES ai_profiles(id),
    version     INTEGER NOT NULL,
    toml        TEXT NOT NULL,
    summary     TEXT NOT NULL,
    created_by  TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    UNIQUE(profile_id, version)
);

CREATE INDEX IF NOT EXISTS idx_ai_profile_versions_profile
    ON ai_profile_versions(profile_id, version DESC);

CREATE TABLE IF NOT EXISTS ai_providers (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL UNIQUE,
    kind         TEXT NOT NULL,
    endpoint     TEXT,
    capabilities TEXT NOT NULL,
    created_by   TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    retired_at   INTEGER
);
