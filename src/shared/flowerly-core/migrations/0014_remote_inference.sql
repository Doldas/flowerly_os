-- Remote inference consent and attempts (ADR-0022, #263).
--
-- Owner Q3: consent belongs to one authenticated user, one provider and one
-- destination host, lasts at most 30 days and is revocable at once.
-- Owner Q4: attempts keep idempotency and accounting metadata only. No
-- prompt or response text is stored: `request_digest` is a SHA-256 of the
-- request arguments, sufficient to detect a reused operation_id with
-- different arguments and nothing more.

CREATE TABLE remote_inference_consents (
    id           TEXT PRIMARY KEY,
    owner_class  TEXT NOT NULL CHECK (owner_class = 'user'),
    owner_uid    INTEGER NOT NULL CHECK (owner_uid BETWEEN 0 AND 4294967294),
    provider_id  TEXT NOT NULL,
    destination  TEXT NOT NULL CHECK (length(destination) BETWEEN 1 AND 253),
    granted_at   INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL CHECK (
        expires_at > granted_at AND expires_at - granted_at <= 30 * 86400000),
    revoked_at   INTEGER
);

CREATE INDEX idx_remote_consents_owner
    ON remote_inference_consents(owner_uid, provider_id, destination);

-- A grant's scope never changes; only revocation may be recorded.
CREATE TRIGGER remote_consents_scope_immutable
BEFORE UPDATE ON remote_inference_consents
WHEN NEW.owner_class IS NOT OLD.owner_class OR NEW.owner_uid IS NOT OLD.owner_uid
    OR NEW.provider_id IS NOT OLD.provider_id OR NEW.destination IS NOT OLD.destination
    OR NEW.granted_at IS NOT OLD.granted_at OR NEW.expires_at IS NOT OLD.expires_at
    OR (OLD.revoked_at IS NOT NULL AND NEW.revoked_at IS NOT OLD.revoked_at)
BEGIN
    SELECT RAISE(ABORT, 'a consent grant can only be revoked, once');
END;

CREATE TABLE remote_inference_attempts (
    id              TEXT PRIMARY KEY,
    owner_class     TEXT NOT NULL CHECK (owner_class = 'user'),
    owner_uid       INTEGER NOT NULL CHECK (owner_uid BETWEEN 0 AND 4294967294),
    workload_id     TEXT NOT NULL REFERENCES inference_workloads(id),
    provider_id     TEXT NOT NULL,
    destination     TEXT NOT NULL,
    model           TEXT NOT NULL,
    consent_id      TEXT NOT NULL REFERENCES remote_inference_consents(id),
    operation_id    TEXT,
    request_id      TEXT NOT NULL,
    request_digest  TEXT NOT NULL CHECK (length(request_digest) = 64),
    state           TEXT NOT NULL CHECK (
        state IN ('submitting', 'pending', 'succeeded', 'failed', 'unknown')),
    outcome         TEXT CHECK (outcome IN ('not_sent', 'sent', 'unknown')),
    error_class     TEXT,
    model_used      TEXT,
    usage_source    TEXT CHECK (usage_source IN ('provider_reported', 'unknown')),
    tokens_in       INTEGER CHECK (tokens_in IS NULL OR tokens_in >= 0),
    tokens_out      INTEGER CHECK (tokens_out IS NULL OR tokens_out >= 0),
    latency_ms      INTEGER CHECK (latency_ms IS NULL OR latency_ms >= 0),
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);

-- One attempt per operation identity per user (NULL operation_ids are
-- distinct in SQLite, so requests without one never collide).
CREATE UNIQUE INDEX idx_remote_attempts_operation
    ON remote_inference_attempts(owner_uid, operation_id);
CREATE INDEX idx_remote_attempts_open
    ON remote_inference_attempts(owner_uid, provider_id, state);

-- Terminal attempts are final: an unknown outcome is never turned into a
-- success or a retry by a later writer.
CREATE TRIGGER remote_attempts_terminal_final
BEFORE UPDATE ON remote_inference_attempts
WHEN OLD.state IN ('succeeded', 'failed', 'unknown')
BEGIN
    SELECT RAISE(ABORT, 'a finished remote inference attempt cannot change');
END;
