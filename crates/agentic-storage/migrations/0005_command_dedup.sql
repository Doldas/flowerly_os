-- Optional caller-supplied operation IDs for retry-safe non-idempotent creates.
-- An operation result and its domain records are inserted in ONE transaction.
-- Rows are intentionally never silently evicted: deleting replay evidence can
-- turn a late retry into a second side effect. A per-actor hard cap fails closed.
CREATE TABLE command_dedup (
    actor TEXT NOT NULL,
    command TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    request_json TEXT NOT NULL,
    response_json TEXT NOT NULL,
    recorded_at INTEGER NOT NULL,
    PRIMARY KEY (actor, command, operation_id)
);
CREATE INDEX command_dedup_actor_created
    ON command_dedup(actor, recorded_at);
