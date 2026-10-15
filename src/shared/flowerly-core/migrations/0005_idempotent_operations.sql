-- Opt-in idempotency registry for SQLite-only create mutations.
-- Each operation row commits atomically with the created domain row.
CREATE TABLE IF NOT EXISTS ipc_operations (
    actor TEXT NOT NULL,
    command TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    request_payload TEXT NOT NULL,
    response_payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (actor, command, operation_id)
);
CREATE INDEX IF NOT EXISTS idx_ipc_operations_created_at ON ipc_operations(created_at);
