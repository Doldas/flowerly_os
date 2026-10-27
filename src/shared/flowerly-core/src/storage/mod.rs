#![forbid(unsafe_code)]

pub mod admin;
pub mod repository;
pub mod scoped;

pub use admin::{
    AdminAuditRecord, OwnedObject, OwnershipReport, MAX_AUDIT_REASON, MAX_OWNERSHIP_COMPONENT,
};
pub use scoped::{FlowView, ProfileView};

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use crate::types::{
    AgentId, Execution, ExecutionId, Flow, FlowId, FlowState, Generation, GenerationId,
    GenerationStatus, Goal, GoalId, GoalStatus, ModelId, ModelRecord, ObjectRef, Owner, ProfileId,
    ProvenanceEdge, ProviderId, ProviderRecord, Session, SessionId, SessionPresentationState,
    SessionState, Timestamp, UsageRecord, WorkloadId, WorkloadRecord, WorkloadState,
};
use rusqlite::types::Type;
use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("record not found: {0}")]
    NotFound(String),

    #[error("unique constraint violation: {0}")]
    UniqueViolation(String),

    #[error("foreign key constraint violation: {0}")]
    ForeignKeyViolation(String),

    #[error("operation conflict: {0}")]
    OperationConflict(String),

    #[error("invalid operation request: {0}")]
    InvalidOperation(String),

    /// Refused for a reason that reveals nothing the caller cannot already
    /// see (for example archiving the shared General Flow).
    #[error("permission denied: {0}")]
    PermissionDenied(String),
}

impl StorageError {
    /// A failed UNIQUE constraint on a business name means the same name was
    /// already taken; the typed IPC mapping for this is `Conflict`.
    #[must_use]
    pub fn is_unique_violation(&self) -> bool {
        matches!(
            self,
            Self::Sqlite(rusqlite::Error::SqliteFailure(error, _))
                if matches!(
                    error.extended_code,
                    rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                        | rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
                )
        ) || matches!(self, Self::UniqueViolation(_))
    }

    /// SQLite writer contention is a transient availability condition, not
    /// corrupt user input or an internal application error.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            Self::Sqlite(rusqlite::Error::SqliteFailure(error, _))
                if matches!(
                    error.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                )
        )
    }

    /// Check if this is a foreign key violation.
    #[must_use]
    pub fn is_foreign_key_violation(&self) -> bool {
        matches!(
            self,
            Self::Sqlite(rusqlite::Error::SqliteFailure(error, _))
                if error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY
        ) || matches!(self, Self::ForeignKeyViolation(_))
    }

    /// The single IPC error classification for storage failures, shared by
    /// every storage-backed command so the mapping cannot drift per daemon.
    ///
    /// Classification uses typed variants and SQLite result codes, never the
    /// error text. Only BUSY/LOCKED contention is transient (`Unavailable`);
    /// corruption, I/O, full disk, read-only databases, CHECK/NOT NULL
    /// violations and decode failures stay `Internal` because retrying cannot
    /// fix them and they usually indicate a defect or a damaged database.
    #[must_use]
    pub fn ipc_error_code(&self) -> crate::ipc::ErrorCode {
        use crate::ipc::ErrorCode;
        match self {
            Self::NotFound(_) => ErrorCode::NotFound,
            Self::PermissionDenied(_) => ErrorCode::PermissionDenied,
            Self::InvalidOperation(_) | Self::ForeignKeyViolation(_) => ErrorCode::InvalidArgument,
            Self::OperationConflict(_) | Self::UniqueViolation(_) => ErrorCode::Conflict,
            Self::Sqlite(_) if self.is_busy() => ErrorCode::Unavailable,
            Self::Sqlite(_) if self.is_unique_violation() => ErrorCode::Conflict,
            Self::Sqlite(_) if self.is_foreign_key_violation() => ErrorCode::InvalidArgument,
            Self::Sqlite(_) | Self::Serde(_) | Self::Io(_) => ErrorCode::Internal,
        }
    }
}

/// Retry configuration for operations that may encounter transient SQLite contention.
#[derive(Debug, Clone, Copy)]
pub struct RetryConfig {
    pub max_attempts: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_delay_ms: 10,
            max_delay_ms: 1000,
        }
    }
}

/// Execute a fallible operation with exponential backoff retry on SQLite busy errors.
pub fn retry_on_busy<T, F>(config: RetryConfig, mut operation: F) -> Result<T>
where
    F: FnMut() -> Result<T>,
{
    let mut attempt = 0;
    let mut delay = config.base_delay_ms;
    loop {
        match operation() {
            Ok(value) => return Ok(value),
            Err(err) => {
                if err.is_busy() && attempt < config.max_attempts - 1 {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(delay));
                    delay = (delay * 2).min(config.max_delay_ms);
                    continue;
                }
                return Err(err);
            }
        }
    }
}

/// Idempotent operation key for retry safety.
/// Operations with the same key are guaranteed to execute at most once.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OperationKey(pub String);

impl OperationKey {
    /// Create a new random operation key.
    #[must_use]
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Create from an existing string (e.g., client-provided idempotency key).
    #[must_use]
    pub fn from_string(s: String) -> Self {
        Self(s)
    }
}

impl Default for OperationKey {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for OperationKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for OperationKey {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Self(s.to_owned()))
    }
}

/// Result of an idempotent operation check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotentResult<T> {
    /// Operation not seen before; caller should execute it.
    Execute,
    /// Operation already completed; return cached result.
    Completed(T),
}

/// Check if an operation has been recorded, recording it if not.
/// Returns `Execute` if the operation is new, `Completed(value)` if already done.
pub fn check_idempotent<T, F>(
    conn: &Connection,
    key: &OperationKey,
    deserialize: F,
) -> Result<IdempotentResult<T>>
where
    F: FnOnce(&str) -> Result<T>,
{
    // Create table if not exists
    conn.execute(
        "CREATE TABLE IF NOT EXISTS idempotent_operations (
            key TEXT PRIMARY KEY,
            result TEXT NOT NULL,
            created_at INTEGER NOT NULL
        )",
        [],
    )?;

    // Try to insert the key (will fail if already exists)
    let now = crate::types::Timestamp::now().as_millis();
    let insert_result = conn.execute(
        "INSERT OR IGNORE INTO idempotent_operations (key, result, created_at) VALUES (?1, ?2, ?3)",
        params![key.to_string(), "pending", now],
    )?;

    if insert_result == 1 {
        // We inserted the key; operation should be executed
        Ok(IdempotentResult::Execute)
    } else {
        // Key already exists; check if result is available
        let result: Option<String> = conn
            .query_row(
                "SELECT result FROM idempotent_operations WHERE key = ?1",
                params![key.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        match result {
            Some(cached) if cached != "pending" => {
                deserialize(&cached).map(IdempotentResult::Completed)
            }
            _ => {
                // Operation is still pending (another process executing it)
                // Treat as "execute" but caller should handle conflict
                Ok(IdempotentResult::Execute)
            }
        }
    }
}

/// Record the result of an idempotent operation.
pub fn record_idempotent<T>(conn: &Connection, key: &OperationKey, result: &T) -> Result<()>
where
    T: Serialize,
{
    let serialized = serde_json::to_string(result)?;
    conn.execute(
        "UPDATE idempotent_operations SET result = ?2 WHERE key = ?1",
        params![key.to_string(), serialized],
    )?;
    Ok(())
}

/// Execute an operation idempotently with SQLite busy retry.
/// If the operation key was already executed, returns the cached result.
pub fn execute_idempotent<T, F>(
    conn: &Connection,
    key: &OperationKey,
    mut operation: F,
) -> Result<T>
where
    F: FnMut() -> Result<T>,
    T: Serialize + for<'de> Deserialize<'de>,
{
    let config = RetryConfig::default();
    let mut attempt = 0;
    let mut delay = config.base_delay_ms;

    loop {
        // Check if already executed
        let check = check_idempotent(conn, key, |s| {
            serde_json::from_str(s).map_err(StorageError::from)
        });
        match check {
            Ok(IdempotentResult::Completed(cached)) => return Ok(cached),
            Ok(IdempotentResult::Execute) => {}
            Err(err) => return Err(err),
        }

        // Execute the operation
        match operation() {
            Ok(value) => {
                // Record success
                record_idempotent(conn, key, &value)?;
                return Ok(value);
            }
            Err(err) => {
                if err.is_busy() && attempt < config.max_attempts - 1 {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(delay));
                    delay = (delay * 2).min(config.max_delay_ms);
                    continue;
                }
                // On non-busy error, don't record (allows retry with same key)
                return Err(err);
            }
        }
    }
}

pub type Result<T> = std::result::Result<T, StorageError>;

#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: i64,

    pub name: &'static str,

    pub sql: &'static str,
}

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "init",
        sql: include_str!("../../migrations/0001_init.sql"),
    },
    Migration {
        version: 2,
        name: "session_presentation_and_actor_refs",
        sql: include_str!("../../migrations/0002_session_presentation_and_actor_refs.sql"),
    },
    Migration {
        version: 3,
        name: "single_foreground_flow",
        sql: include_str!("../../migrations/0003_single_foreground_flow.sql"),
    },
    Migration {
        version: 4,
        name: "protect_general",
        sql: include_str!("../../migrations/0004_protect_general.sql"),
    },
    Migration {
        version: 5,
        name: "idempotent_operations",
        sql: include_str!("../../migrations/0005_idempotent_operations.sql"),
    },
    Migration {
        version: 6,
        name: "model_catalog",
        sql: include_str!("../../migrations/0006_model_catalog.sql"),
    },
    Migration {
        version: 7,
        name: "profiles_providers",
        sql: include_str!("../../migrations/0007_profiles_providers.sql"),
    },
    Migration {
        version: 8,
        name: "workloads_usage",
        sql: include_str!("../../migrations/0008_workloads_usage.sql"),
    },
    Migration {
        version: 9,
        name: "object_ownership",
        sql: include_str!("../../migrations/0009_object_ownership.sql"),
    },
    Migration {
        version: 10,
        name: "per_owner_foreground",
        sql: include_str!("../../migrations/0010_per_owner_foreground.sql"),
    },
    Migration {
        version: 11,
        name: "generation_reason_goal_owner",
        sql: include_str!("../../migrations/0011_generation_reason_goal_owner.sql"),
    },
    Migration {
        version: 12,
        name: "compute_ownership",
        sql: include_str!("../../migrations/0012_compute_ownership.sql"),
    },
    Migration {
        version: 13,
        name: "generation_ownership_admin_audit",
        sql: include_str!("../../migrations/0013_generation_ownership_admin_audit.sql"),
    },
];

/// Live domain counts reported by service Status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StatusCounts {
    pub flows: u64,
    pub goals: u64,
    pub sessions: u64,
    pub executions: u64,
    pub generations: u64,
    pub provenance_edges: u64,
    pub models: u64,
    pub profiles: u64,
    pub providers: u64,
    pub active_workloads: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMigration {
    pub version: i64,

    pub name: String,

    pub applied_at: Timestamp,
}

/// Optional, actor-bound key for an atomic SQLite create operation.
/// The caller must use a kernel-attested actor, not an arbitrary client label.
pub struct MutationOperation<'a> {
    pub actor: &'a str,
    pub command: &'a str,
    pub operation_id: &'a str,
    pub request_payload: &'a str,
    pub response_payload: &'a str,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        Self::configure(&conn)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::configure(&conn)?;
        Ok(Self { conn })
    }

    fn configure(conn: &Connection) -> Result<()> {
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(())
    }

    pub fn migrate(&mut self) -> Result<Vec<AppliedMigration>> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                 version    INTEGER PRIMARY KEY,
                 name       TEXT NOT NULL,
                 applied_at INTEGER NOT NULL
             );",
        )?;

        let mut applied = Vec::new();
        for migration in MIGRATIONS {
            // A shared state.db means several daemons may start at the same time.
            // IMMEDIATE serializes migration writers; the applied-version check
            // happens inside the transaction so a daemon that waited for another
            // migrator observes the committed version before doing any work.
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let exists = tx
                .query_row(
                    "SELECT version FROM schema_migrations WHERE version = ?1",
                    params![migration.version],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .is_some();

            if exists {
                tx.commit()?;
                continue;
            }

            let tx_record = Timestamp::now();
            tx.execute_batch(migration.sql)?;
            tx.execute(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                params![migration.version, migration.name, tx_record.as_millis()],
            )?;
            tx.commit()?;
            tracing::info!(
                migration.version,
                migration.name,
                "applied database migration"
            );
            applied.push(AppliedMigration {
                version: migration.version,
                name: migration.name.to_owned(),
                applied_at: tx_record,
            });
        }
        Ok(applied)
    }

    pub fn applied_migrations(&self) -> Result<Vec<AppliedMigration>> {
        let mut stmt = self
            .conn
            .prepare("SELECT version, name, applied_at FROM schema_migrations ORDER BY version")?;
        let rows = stmt.query_map([], |row| {
            Ok(AppliedMigration {
                version: row.get(0)?,
                name: row.get(1)?,
                applied_at: Timestamp::from_millis(row.get(2)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn health_check(&self) -> Result<()> {
        let value: i64 = self.conn.query_row("SELECT 1", [], |row| row.get(0))?;
        if value != 1 {
            return Err(StorageError::NotFound("database health check".into()));
        }
        Ok(())
    }

    /// Live per-domain row counts read in one statement, so every count comes
    /// from the same snapshot. Filters match the corresponding `list_*` calls.
    pub fn status_counts(&self) -> Result<StatusCounts> {
        let count = |row: &Row<'_>, index: usize| -> rusqlite::Result<u64> {
            let value: i64 = row.get(index)?;
            u64::try_from(value).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(index, Type::Integer, Box::new(error))
            })
        };
        let counts = self.conn.query_row(
            "SELECT
                 (SELECT COUNT(*) FROM flows),
                 (SELECT COUNT(*) FROM goals),
                 (SELECT COUNT(*) FROM sessions),
                 (SELECT COUNT(*) FROM executions),
                 (SELECT COUNT(*) FROM generations),
                 (SELECT COUNT(*) FROM provenance_edges),
                 (SELECT COUNT(*) FROM models WHERE retired_at IS NULL),
                 (SELECT COUNT(*) FROM ai_profiles),
                 (SELECT COUNT(*) FROM ai_providers WHERE retired_at IS NULL),
                 (SELECT COUNT(*) FROM inference_workloads WHERE state IN ('QUEUED', 'ADMITTED'))",
            [],
            |row| {
                Ok(StatusCounts {
                    flows: count(row, 0)?,
                    goals: count(row, 1)?,
                    sessions: count(row, 2)?,
                    executions: count(row, 3)?,
                    generations: count(row, 4)?,
                    provenance_edges: count(row, 5)?,
                    models: count(row, 6)?,
                    profiles: count(row, 7)?,
                    providers: count(row, 8)?,
                    active_workloads: count(row, 9)?,
                })
            },
        )?;
        Ok(counts)
    }

    /// Bootstrap or recover the unique General system Flow transactionally.
    /// A pre-existing General keeps its durable ID, including legacy IDs.
    /// For a fresh database General has a deterministic ID. An archived
    /// General is repaired. Foregrounds are per owner (`flow_foreground`)
    /// and are never touched here: a uid without a row sees General.
    pub fn ensure_general_flow(&mut self) -> Result<FlowId> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<String> = tx
            .query_row("SELECT id FROM flows WHERE name = 'General'", [], |row| {
                row.get(0)
            })
            .optional()?;
        let id = if let Some(raw) = existing {
            FlowId::from_str(&raw).map_err(|error| {
                StorageError::Sqlite(rusqlite::Error::InvalidParameterName(error.to_string()))
            })?
        } else {
            let mut general = Flow::new("General", "system");
            general.owner = Owner::System;
            general.id = FlowId::from_raw("flow-general");
            general.purpose = Some("The universal default context".to_owned());
            insert_flow_row(&tx, &general)?;
            insert_provenance_row(
                &tx,
                &ProvenanceEdge {
                    from: ObjectRef::from(&general.id),
                    relation: crate::types::ProvenanceRelation::CreatedBy,
                    to: ObjectRef::actor("system").map_err(|error| {
                        StorageError::Sqlite(rusqlite::Error::InvalidParameterName(
                            error.to_string(),
                        ))
                    })?,
                    created_at: Timestamp::now(),
                    note: Some("bootstrap default General flow".to_owned()),
                },
            )?;
            general.id
        };
        tx.execute(
            "UPDATE flows SET state = 'ACTIVE', revision = revision + 1
             WHERE id = ?1 AND state = 'ARCHIVED'",
            params![id.as_str()],
        )?;
        // General is the shared system Flow by definition (#225).
        tx.execute(
            "UPDATE flows SET owner_class = 'system', owner_uid = NULL
             WHERE id = ?1 AND owner_class != 'system'",
            params![id.as_str()],
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Replays the original response for the same actor/command/key/payload.
    /// Each accepted operation and domain write commit in one IMMEDIATE transaction.
    /// An expired key can be reused after the retention window; clients must not
    /// assume infinite-time exactly-once semantics.
    fn insert_once(
        &mut self,
        operation: &MutationOperation<'_>,
        write: impl FnOnce(&Connection) -> Result<()>,
    ) -> Result<String> {
        let response = operation.response_payload;
        self.run_once(operation, |conn| {
            write(conn)?;
            Ok(response.to_owned())
        })
    }

    /// Like `insert_once`, but the response is produced by `write` inside the
    /// transaction, for mutations whose result (such as an allocated version
    /// number) is only known once the write runs. `response_payload` of the
    /// operation is ignored.
    fn run_once(
        &mut self,
        operation: &MutationOperation<'_>,
        write: impl FnOnce(&Connection) -> Result<String>,
    ) -> Result<String> {
        let id = operation.operation_id;
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || b"._:-".contains(&ch))
        {
            return Err(StorageError::InvalidOperation(
                "operation_id must use 1..128 ASCII letters, numbers, ., _, :, or -".into(),
            ));
        }
        if operation.request_payload.len() > 64 * 1024 {
            return Err(StorageError::InvalidOperation(
                "idempotent operation payload exceeds 64 KiB".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Thirty days bounds retained request bodies and operation results.
        // Expiry cannot guarantee protection from retries beyond this window.
        let expires_before = Timestamp::now().as_millis() - 30_i64 * 24 * 60 * 60 * 1000;
        tx.execute(
            "DELETE FROM ipc_operations WHERE created_at < ?1 AND command != 'session.spawn'",
            params![expires_before],
        )?;
        let recorded: Option<(String, String)> = tx
            .query_row(
                "SELECT request_payload, response_payload FROM ipc_operations
                 WHERE actor = ?1 AND command = ?2 AND operation_id = ?3",
                params![operation.actor, operation.command, id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((prior_request, prior_response)) = recorded {
            if prior_request != operation.request_payload {
                return Err(StorageError::OperationConflict(
                    "operation_id was already used with different request arguments".into(),
                ));
            }
            tx.commit()?;
            return Ok(prior_response);
        }
        let response = write(&tx)?;
        if response.len() > 64 * 1024 {
            return Err(StorageError::InvalidOperation(
                "idempotent operation payload exceeds 64 KiB".into(),
            ));
        }
        tx.execute(
            "INSERT INTO ipc_operations
             (actor, command, operation_id, request_payload, response_payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                operation.actor,
                operation.command,
                id,
                operation.request_payload,
                response,
                Timestamp::now().as_millis(),
            ],
        )?;
        tx.commit()?;
        Ok(response)
    }

    /// Reserve an externally effective PTY operation BEFORE launching anything.
    /// A reservation is never pruned by normal 30-day mutation housekeeping:
    /// expiring one could silently launch a second process on a late retry.
    /// A repeated reservation is not proof the guardian survived; callers must
    /// answer unavailable until recovery is independently proven.
    pub fn reserve_external_spawn(
        &mut self,
        actor: &str,
        operation_id: &str,
        request_payload: &str,
        session_id: &SessionId,
    ) -> Result<bool> {
        if operation_id.is_empty()
            || operation_id.len() > 128
            || !operation_id
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || b"._:-".contains(&ch))
            || request_payload.len() > 64 * 1024
        {
            return Err(StorageError::InvalidOperation(
                "invalid external operation_id or request payload".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<String> = tx
            .query_row(
                "SELECT request_payload FROM ipc_operations
             WHERE actor = ?1 AND command = 'session.spawn' AND operation_id = ?2",
                params![actor, operation_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(previous) = previous {
            if previous != request_payload {
                return Err(StorageError::OperationConflict(
                    "operation_id was already used with different spawn arguments".into(),
                ));
            }
            tx.commit()?;
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO ipc_operations
             (actor, command, operation_id, request_payload, response_payload, created_at)
             VALUES (?1, 'session.spawn', ?2, ?3, ?4, ?5)",
            params![
                actor,
                operation_id,
                request_payload,
                session_id.as_str(),
                Timestamp::now().as_millis()
            ],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Query the durable operation-to-Session identity. This does not imply
    /// that the external process survived or that the caller owns its FDs.
    pub fn lookup_external_spawn(
        &self,
        actor: &str,
        operation_id: &str,
    ) -> Result<Option<SessionId>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT response_payload FROM ipc_operations
             WHERE actor = ?1 AND command = 'session.spawn' AND operation_id = ?2",
                params![actor, operation_id],
                |row| row.get(0),
            )
            .optional()?;
        match raw {
            Some(raw) => {
                Ok(Some(SessionId::from_str(&raw).map_err(|error| {
                    StorageError::InvalidOperation(error.to_string())
                })?))
            }
            None => Ok(None),
        }
    }

    /// Durably record a spawn WITHOUT an operation_id before launching it
    /// (#74), so restart recovery can find its cgroup even if the daemon,
    /// guardian and cleanup watcher all die before the Session row commits.
    /// It is not a retry identity: nothing can look it up, and it is released
    /// once the Session row exists (`release_unkeyed_spawn`).
    pub fn reserve_unkeyed_spawn(&mut self, actor: &str, session_id: &SessionId) -> Result<()> {
        self.conn.execute(
            "INSERT INTO ipc_operations
             (actor, command, operation_id, request_payload, response_payload, created_at)
             VALUES (?1, 'session.spawn.unkeyed', ?2, '{}', ?2, ?3)",
            params![actor, session_id.as_str(), Timestamp::now().as_millis()],
        )?;
        Ok(())
    }

    /// The Session row now identifies the cgroup; drop the launch record.
    pub fn release_unkeyed_spawn(&mut self, session_id: &SessionId) -> Result<()> {
        self.conn.execute(
            "DELETE FROM ipc_operations
             WHERE command = 'session.spawn.unkeyed' AND operation_id = ?1",
            params![session_id.as_str()],
        )?;
        Ok(())
    }

    /// After restart recovery has killed their cgroups, unkeyed launch
    /// records carry no further meaning: no client can look them up.
    pub fn clear_unkeyed_spawn_reservations(&mut self) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM ipc_operations WHERE command = 'session.spawn.unkeyed'",
            [],
        )?)
    }

    /// Includes reserved IDs without Session rows: the daemon may have died
    /// between launching a guardian and persisting the Session result.
    /// Keyed reservations are kept forever; unkeyed ones until recovery.
    pub fn list_external_spawn_reservations(&self) -> Result<Vec<SessionId>> {
        let mut statement = self.conn.prepare(
            "SELECT response_payload FROM ipc_operations
             WHERE command IN ('session.spawn', 'session.spawn.unkeyed')",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut ids = Vec::new();
        for row in rows {
            let raw = row?;
            ids.push(
                SessionId::from_str(&raw)
                    .map_err(|error| StorageError::InvalidOperation(error.to_string()))?,
            );
        }
        Ok(ids)
    }

    /// Durable, actor-scoped opt-in retry identity for Flow creation.
    pub fn insert_flow_with_provenance_once(
        &mut self,
        flow: &Flow,
        edges: &[ProvenanceEdge],
        operation: &MutationOperation<'_>,
    ) -> Result<String> {
        if operation.actor != flow.created_by {
            return Err(StorageError::OperationConflict(
                "operation actor does not match durable creator".into(),
            ));
        }
        self.insert_once(operation, |conn| {
            insert_flow_row(conn, flow)?;
            for edge in edges {
                insert_provenance_row(conn, edge)?;
            }
            Ok(())
        })
    }

    pub fn insert_goal_with_provenance_once(
        &mut self,
        goal: &Goal,
        edges: &[ProvenanceEdge],
        operation: &MutationOperation<'_>,
    ) -> Result<String> {
        if operation.actor != goal.created_by {
            return Err(StorageError::OperationConflict(
                "operation actor does not match durable creator".into(),
            ));
        }
        self.insert_once(operation, |conn| {
            check_goal_references(conn, goal)?;
            insert_goal_row(conn, goal)?;
            for edge in edges {
                insert_provenance_row(conn, edge)?;
            }
            Ok(())
        })
    }

    pub fn insert_session_with_provenance_once(
        &mut self,
        session: &Session,
        edges: &[ProvenanceEdge],
        operation: &MutationOperation<'_>,
    ) -> Result<String> {
        if operation.actor != session.created_by {
            return Err(StorageError::OperationConflict(
                "operation actor does not match durable creator".into(),
            ));
        }
        self.insert_once(operation, |conn| {
            check_session_references(conn, session)?;
            insert_session_row(conn, session)?;
            for edge in edges {
                insert_provenance_row(conn, edge)?;
            }
            Ok(())
        })
    }

    /// Durable, actor-scoped opt-in retry identity for model registration.
    pub fn insert_model_with_provenance(
        &mut self,
        model: &ModelRecord,
        edges: &[ProvenanceEdge],
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_model_row(&tx, model)?;
        for edge in edges {
            insert_provenance_row(&tx, edge)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn insert_model_with_provenance_once(
        &mut self,
        model: &ModelRecord,
        edges: &[ProvenanceEdge],
        operation: &MutationOperation<'_>,
    ) -> Result<String> {
        if operation.actor != model.created_by {
            return Err(StorageError::OperationConflict(
                "operation actor does not match durable creator".into(),
            ));
        }
        self.insert_once(operation, |conn| {
            insert_model_row(conn, model)?;
            for edge in edges {
                insert_provenance_row(conn, edge)?;
            }
            Ok(())
        })
    }

    /// Create a profile record with its first append-only version in one
    /// immediate transaction. When an idempotent operation is supplied the
    /// stored response JSON is replayed; a fresh creation returns an empty
    /// string so callers can distinguish the two.
    #[allow(clippy::too_many_arguments)]
    pub fn create_profile(
        &mut self,
        id: &ProfileId,
        name: &str,
        created_by: &str,
        toml: &str,
        summary: &str,
        owner: Owner,
        operation: Option<MutationOperation<'_>>,
        edges: &[ProvenanceEdge],
    ) -> Result<String> {
        let now = Timestamp::now();
        let write = |conn: &Connection| -> Result<()> {
            conn.execute(
                "INSERT INTO ai_profiles (id, name, created_by, created_at, owner_class, owner_uid)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id.as_str(),
                    name,
                    created_by,
                    now.as_millis(),
                    owner.class(),
                    owner.uid()
                ],
            )?;
            conn.execute(
                "INSERT INTO ai_profile_versions
                     (profile_id, version, toml, summary, created_by, created_at)
                 VALUES (?1, 1, ?2, ?3, ?4, ?5)",
                params![id.as_str(), toml, summary, created_by, now.as_millis()],
            )?;
            for edge in edges {
                insert_provenance_row(conn, edge)?;
            }
            Ok(())
        };
        match operation {
            Some(operation) => self.insert_once(&operation, write),
            None => {
                let tx = self
                    .conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                write(&tx)?;
                tx.commit()?;
                Ok(String::new())
            }
        }
    }

    /// Register a provider. Remote kinds never pass storage here; the daemon
    /// must refuse them against egress policy before calling this.
    #[allow(clippy::too_many_arguments)]
    pub fn register_provider(
        &mut self,
        id: &ProviderId,
        name: &str,
        kind: &str,
        endpoint: Option<&str>,
        capabilities: &[String],
        created_by: &str,
        operation: Option<MutationOperation<'_>>,
        edges: &[ProvenanceEdge],
    ) -> Result<String> {
        let now = Timestamp::now();
        let write = |conn: &Connection| -> Result<()> {
            conn.execute(
                // Providers are registered only through the Admin-classified
                // provider.register, so they are system definitions (#225).
                "INSERT INTO ai_providers
                     (id, name, kind, endpoint, capabilities, created_by, created_at, owner_class)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'system')",
                params![
                    id.as_str(),
                    name,
                    kind,
                    endpoint,
                    serde_json::to_string(&capabilities)?,
                    created_by,
                    now.as_millis(),
                ],
            )?;
            for edge in edges {
                insert_provenance_row(conn, edge)?;
            }
            Ok(())
        };
        match operation {
            Some(operation) => self.insert_once(&operation, write),
            None => {
                let tx = self
                    .conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                write(&tx)?;
                tx.commit()?;
                Ok(String::new())
            }
        }
    }

    pub fn list_providers(&self) -> Result<Vec<ProviderRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, endpoint, capabilities, created_by, created_at, retired_at
             FROM ai_providers WHERE retired_at IS NULL ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], map_provider)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn get_provider(&self, id: &ProviderId) -> Result<Option<ProviderRecord>> {
        self.conn
            .query_row(
                "SELECT id, name, kind, endpoint, capabilities, created_by, created_at, retired_at
                 FROM ai_providers WHERE id = ?1",
                params![id.as_str()],
                map_provider,
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// Record one admission decision in an immediate transaction together
    /// with its provenance edge.
    pub fn insert_workload(
        &mut self,
        workload: &WorkloadRecord,
        edges: &[ProvenanceEdge],
        operation: Option<MutationOperation<'_>>,
    ) -> Result<String> {
        let write = |conn: &Connection| -> Result<()> {
            conn.execute(
                "INSERT INTO inference_workloads
                     (id, profile_id, profile_name, model_id, background, priority,
                      state, reason, ceilings, created_by, created_at,
                      admitted_at, stopped_at, completed_at, operation_id,
                      owner_class, owner_uid)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                         ?16, ?17)",
                params![
                    workload.id.as_str(),
                    workload.profile_id.as_str(),
                    workload.profile_name,
                    workload.model_id.as_str(),
                    workload.background.as_str(),
                    workload.priority,
                    workload.state.to_string(),
                    workload.reason,
                    serde_json::to_string(&workload.ceilings)?,
                    workload.created_by,
                    workload.created_at.as_millis(),
                    workload.admitted_at.map(|t| t.as_millis()),
                    workload.stopped_at.map(|t| t.as_millis()),
                    workload.completed_at.map(|t| t.as_millis()),
                    workload.operation_id,
                    workload.owner.class(),
                    workload.owner.uid(),
                ],
            )?;
            for edge in edges {
                insert_provenance_row(conn, edge)?;
            }
            Ok(())
        };
        match operation {
            Some(operation) => self.insert_once(&operation, write),
            None => {
                let tx = self
                    .conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                write(&tx)?;
                tx.commit()?;
                Ok(String::new())
            }
        }
    }

    pub fn get_workload(&self, id: &WorkloadId) -> Result<Option<WorkloadRecord>> {
        self.conn
            .query_row(
                "SELECT id, profile_id, profile_name, model_id, background, priority,
                        state, reason, ceilings, created_by, created_at,
                        admitted_at, stopped_at, completed_at, operation_id, owner_class, owner_uid
                 FROM inference_workloads WHERE id = ?1",
                params![id.as_str()],
                map_workload,
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// Record one usage ledger entry; the workload must exist.
    pub fn insert_usage(&mut self, usage: &UsageRecord) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if load_workload_state(&tx, &usage.workload_id)?.is_none() {
            return Err(StorageError::NotFound(format!(
                "workload not found: {}",
                usage.workload_id
            )));
        }
        tx.execute(
            "INSERT INTO inference_usage
                 (id, workload_id, model_id, tokens_in, tokens_out, latency_ms,
                  cost_micros, created_by, created_at, owner_class, owner_uid)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                usage.id.as_str(),
                usage.workload_id.as_str(),
                usage.model_id.as_str(),
                usage.tokens_in as i64,
                usage.tokens_out as i64,
                usage.latency_ms as i64,
                usage.cost_micros as i64,
                usage.created_by,
                usage.created_at.as_millis(),
                usage.owner.class(),
                usage.owner.uid(),
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Usage ledger write with unknown-outcome replay (#63): the stored
    /// response JSON is returned on retry of the same operation identity.
    pub fn insert_usage_once(
        &mut self,
        usage: &UsageRecord,
        operation: MutationOperation<'_>,
    ) -> Result<String> {
        let write = |conn: &Connection| -> Result<()> {
            if load_workload_state(conn, &usage.workload_id)?.is_none() {
                return Err(StorageError::NotFound(format!(
                    "workload not found: {}",
                    usage.workload_id
                )));
            }
            conn.execute(
                "INSERT INTO inference_usage
                     (id, workload_id, model_id, tokens_in, tokens_out, latency_ms,
                      cost_micros, created_by, created_at, owner_class, owner_uid)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    usage.id.as_str(),
                    usage.workload_id.as_str(),
                    usage.model_id.as_str(),
                    usage.tokens_in as i64,
                    usage.tokens_out as i64,
                    usage.latency_ms as i64,
                    usage.cost_micros as i64,
                    usage.created_by,
                    usage.created_at.as_millis(),
                    usage.owner.class(),
                    usage.owner.uid(),
                ],
            )?;
            Ok(())
        };
        self.insert_once(&operation, write)
    }

    /// Aggregate usage per model plus the caller-provided ceilings, so the
    /// budget view can compare totals with profile ceilings honestly.
    pub fn usage_totals_per_model(&self, model_id: &ModelId) -> Result<(u64, u64, u64)> {
        self.conn
            .query_row(
                "SELECT COALESCE(SUM(tokens_in), 0), COALESCE(SUM(tokens_out), 0),
                    COALESCE(SUM(cost_micros), 0)
             FROM inference_usage WHERE model_id = ?1",
                params![model_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)? as u64,
                        row.get::<_, i64>(1)? as u64,
                        row.get::<_, i64>(2)? as u64,
                    ))
                },
            )
            .map_err(StorageError::from)
    }

    pub fn get_model(&self, id: &ModelId) -> Result<Option<ModelRecord>> {
        self.conn
            .query_row(
                "SELECT id, name, family, architecture, backend, context_tokens,
                        measured_memory_bytes, modalities, license, evidence,
                        created_by, created_at, retired_at
                 FROM models WHERE id = ?1",
                params![id.as_str()],
                map_model,
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// Active (non-retired) catalog rows only; retired rows keep provenance.
    pub fn list_models(&self) -> Result<Vec<ModelRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, family, architecture, backend, context_tokens,
                    measured_memory_bytes, modalities, license, evidence,
                    created_by, created_at, retired_at
             FROM models WHERE retired_at IS NULL ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], map_model)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Soft-retire transactionally. A missing id is a typed NotFound;
    /// an already retired id stays retirement without a duplicate edge.
    pub fn retire_model(&mut self, id: &ModelId, retired_by: &str) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE models
                 SET retired_at = ?2
                 WHERE id = ?1 AND retired_at IS NULL",
            params![id.as_str(), Timestamp::now().as_millis()],
        )?;
        if changed == 0 {
            let exists: Option<String> = tx
                .query_row(
                    "SELECT id FROM models WHERE id = ?1",
                    params![id.as_str()],
                    |row| row.get(0),
                )
                .optional()?;
            tx.commit()?;
            return exists
                .map(|_| ())
                .ok_or_else(|| StorageError::NotFound(format!("model not found: {id}")));
        }
        insert_provenance_row(
            &tx,
            &ProvenanceEdge {
                from: ObjectRef::from(id),
                relation: crate::types::ProvenanceRelation::CausedBy,
                to: ObjectRef::actor(format!("retire:{retired_by}")).map_err(|error| {
                    StorageError::Sqlite(rusqlite::Error::InvalidParameterName(error.to_string()))
                })?,
                created_at: Timestamp::now(),
                note: Some("model catalog retirement".to_owned()),
            },
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn insert_flow(&self, flow: &Flow) -> Result<()> {
        retry_on_busy(RetryConfig::default(), || insert_flow_row(&self.conn, flow))
    }

    pub fn insert_flow_with_provenance(
        &mut self,
        flow: &Flow,
        edges: &[ProvenanceEdge],
    ) -> Result<()> {
        retry_on_busy(RetryConfig::default(), || {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            insert_flow_row(&tx, flow)?;
            for edge in edges {
                insert_provenance_row(&tx, edge)?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn get_flow(&self, id: &FlowId) -> Result<Option<Flow>> {
        self.conn
            .query_row(
                "SELECT id, name, purpose, parent_flow, state, created_by, created_at, last_active, revision, owner_class, owner_uid
                 FROM flows WHERE id = ?1",
                params![id.as_str()],
                map_flow,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn list_flows(&self) -> Result<Vec<Flow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, purpose, parent_flow, state, created_by, created_at, last_active, revision, owner_class, owner_uid
             FROM flows ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], map_flow)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Unscoped lifecycle change for internal and test use only; IPC handlers
    /// use the owner-scoped [`Store::archive_flow_for`].
    pub fn set_flow_state(&self, id: &FlowId, state: FlowState) -> Result<()> {
        retry_on_busy(RetryConfig::default(), || {
            let changed = self.conn.execute(
                "UPDATE flows SET state = ?2 WHERE id = ?1",
                params![id.as_str(), state.to_string()],
            )?;
            if changed == 0 {
                return Err(StorageError::NotFound(id.to_string()));
            }
            Ok(())
        })
    }

    pub fn insert_goal(&self, goal: &Goal) -> Result<()> {
        retry_on_busy(RetryConfig::default(), || insert_goal_row(&self.conn, goal))
    }

    pub fn insert_goal_with_provenance(
        &mut self,
        goal: &Goal,
        edges: &[ProvenanceEdge],
    ) -> Result<()> {
        retry_on_busy(RetryConfig::default(), || {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            check_goal_references(&tx, goal)?;
            insert_goal_row(&tx, goal)?;
            for edge in edges {
                insert_provenance_row(&tx, edge)?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn get_goal(&self, id: &GoalId) -> Result<Option<Goal>> {
        self.conn
            .query_row(
                "SELECT id, parent_goal, objective, rationale, constraints, success_metrics, priority, status, created_by, created_at, owner_class, owner_uid
                 FROM goals WHERE id = ?1",
                params![id.as_str()],
                map_goal,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn list_goals(&self) -> Result<Vec<Goal>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_goal, objective, rationale, constraints, success_metrics, priority, status, created_by, created_at, owner_class, owner_uid
             FROM goals ORDER BY priority DESC, created_at",
        )?;
        let rows = stmt.query_map([], map_goal)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn set_goal_status(&self, id: &GoalId, status: GoalStatus) -> Result<()> {
        retry_on_busy(RetryConfig::default(), || {
            let changed = self.conn.execute(
                "UPDATE goals SET status = ?2 WHERE id = ?1",
                params![id.as_str(), status.to_string()],
            )?;
            if changed == 0 {
                return Err(StorageError::NotFound(id.to_string()));
            }
            Ok(())
        })
    }

    pub fn insert_session(&self, session: &Session) -> Result<()> {
        retry_on_busy(RetryConfig::default(), || {
            insert_session_row(&self.conn, session)
        })
    }

    pub fn insert_session_with_provenance(
        &mut self,
        session: &Session,
        edges: &[ProvenanceEdge],
    ) -> Result<()> {
        retry_on_busy(RetryConfig::default(), || {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            check_session_references(&tx, session)?;
            insert_session_row(&tx, session)?;
            for edge in edges {
                insert_provenance_row(&tx, edge)?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn get_session(&self, id: &SessionId) -> Result<Option<Session>> {
        self.conn
            .query_row(
                "SELECT id, flow_id, kind, pid, cwd, state, presentation_state, created_by, created_at, owner_class, owner_uid
                 FROM sessions WHERE id = ?1",
                params![id.as_str()],
                map_session,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, flow_id, kind, pid, cwd, state, presentation_state, created_by, created_at, owner_class, owner_uid
             FROM sessions ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], map_session)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn update_session_state(&self, id: &SessionId, state: SessionState) -> Result<()> {
        retry_on_busy(RetryConfig::default(), || {
            let changed = self.conn.execute(
                "UPDATE sessions SET state = ?2 WHERE id = ?1",
                params![id.as_str(), state.to_string()],
            )?;
            if changed == 0 {
                return Err(StorageError::NotFound(id.to_string()));
            }
            Ok(())
        })
    }

    pub fn update_session_presentation_state(
        &self,
        id: &SessionId,
        state: SessionPresentationState,
    ) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE sessions SET presentation_state = ?2 WHERE id = ?1",
            params![id.as_str(), state.to_string()],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound(id.to_string()));
        }
        Ok(())
    }

    pub fn insert_execution(&self, exec: &Execution) -> Result<()> {
        retry_on_busy(RetryConfig::default(), || {
            insert_execution_row(&self.conn, exec)
        })
    }

    pub fn insert_execution_with_provenance(
        &mut self,
        exec: &Execution,
        edges: &[ProvenanceEdge],
    ) -> Result<()> {
        retry_on_busy(RetryConfig::default(), || {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            check_execution_references(&tx, exec)?;
            insert_execution_row(&tx, exec)?;
            for edge in edges {
                insert_provenance_row(&tx, edge)?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn get_execution(&self, id: &ExecutionId) -> Result<Option<Execution>> {
        self.conn
            .query_row(
                "SELECT id, agent_id, goal_id, reason, command, exit_code, duration_ms, started_at, owner_class, owner_uid
                 FROM executions WHERE id = ?1",
                params![id.as_str()],
                map_execution,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn list_executions(&self) -> Result<Vec<Execution>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, agent_id, goal_id, reason, command, exit_code, duration_ms, started_at, owner_class, owner_uid
             FROM executions ORDER BY started_at",
        )?;
        let rows = stmt.query_map([], map_execution)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn insert_generation(&self, generation: &Generation) -> Result<()> {
        retry_on_busy(RetryConfig::default(), || {
            insert_generation_row(&self.conn, generation)
        })
    }

    pub fn insert_generation_with_provenance(
        &mut self,
        generation: &Generation,
        edges: &[ProvenanceEdge],
    ) -> Result<()> {
        retry_on_busy(RetryConfig::default(), || {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            if let Some(goal) = &generation.reason_goal {
                scoped::require_same_owner_goal(&tx, generation.owner, goal)?;
            }
            if let Some(parent) = &generation.parent {
                scoped::require_visible_parent_generation(&tx, generation.owner, parent)?;
            }
            insert_generation_row(&tx, generation)?;
            for edge in edges {
                insert_provenance_row(&tx, edge)?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn get_generation(&self, id: &GenerationId) -> Result<Option<Generation>> {
        self.conn
            .query_row(
                "SELECT id, parent, source_commit, system_manifest, reason_goal, experiment, status, created_at, owner_class, owner_uid
                 FROM generations WHERE id = ?1",
                params![id.as_str()],
                map_generation,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn list_generations(&self) -> Result<Vec<Generation>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent, source_commit, system_manifest, reason_goal, experiment, status, created_at, owner_class, owner_uid
             FROM generations ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], map_generation)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn set_generation_status(&self, id: &GenerationId, status: GenerationStatus) -> Result<()> {
        retry_on_busy(RetryConfig::default(), || {
            let changed = self.conn.execute(
                "UPDATE generations SET status = ?2 WHERE id = ?1",
                params![id.as_str(), status.to_string()],
            )?;
            if changed == 0 {
                return Err(StorageError::NotFound(id.to_string()));
            }
            Ok(())
        })
    }

    /// Promote one generation to STABLE and supersede any previous STABLE
    /// generation in the same SQLite transaction.
    pub fn activate_generation(&mut self, id: &GenerationId) -> Result<()> {
        retry_on_busy(RetryConfig::default(), || {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;

            let exists = tx
                .query_row(
                    "SELECT id FROM generations WHERE id = ?1",
                    params![id.as_str()],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .is_some();
            if !exists {
                return Err(StorageError::NotFound(id.to_string()));
            }
            // A private candidate never becomes the system STABLE (#225):
            // it must be promoted, with an audit record, first.
            let owner_class: String = tx.query_row(
                "SELECT owner_class FROM generations WHERE id = ?1",
                params![id.as_str()],
                |row| row.get(0),
            )?;
            if owner_class != "system" {
                return Err(StorageError::OperationConflict(format!(
                    "generation {id} is not a system Generation; promote it first"
                )));
            }

            tx.execute(
                "UPDATE generations
                 SET status = ?1
                 WHERE status = ?2 AND id <> ?3",
                params![
                    GenerationStatus::Superseded.to_string(),
                    GenerationStatus::Stable.to_string(),
                    id.as_str(),
                ],
            )?;
            tx.execute(
                "UPDATE generations SET status = ?2 WHERE id = ?1",
                params![id.as_str(), GenerationStatus::Stable.to_string()],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn insert_provenance(&self, edge: &ProvenanceEdge) -> Result<()> {
        insert_provenance_row(&self.conn, edge)
    }

    pub fn provenance_from(&self, from: &ObjectRef) -> Result<Vec<ProvenanceEdge>> {
        let mut stmt = self.conn.prepare(
            "SELECT from_ref, relation, to_ref, created_at, note
             FROM provenance_edges WHERE from_ref = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![from.as_str()], map_provenance)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn provenance_to(&self, to: &ObjectRef) -> Result<Vec<ProvenanceEdge>> {
        let mut stmt = self.conn.prepare(
            "SELECT from_ref, relation, to_ref, created_at, note
             FROM provenance_edges WHERE to_ref = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![to.as_str()], map_provenance)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

/// Same-owner references, checked in the inserting transaction (#225).
fn check_goal_references(conn: &Connection, goal: &Goal) -> Result<()> {
    match &goal.parent_goal {
        Some(parent) => scoped::require_same_owner_goal(conn, goal.owner, parent),
        None => Ok(()),
    }
}

fn check_session_references(conn: &Connection, session: &Session) -> Result<()> {
    match &session.flow_id {
        Some(flow) => scoped::require_usable_flow(conn, session.owner, flow),
        None => Ok(()),
    }
}

fn check_execution_references(conn: &Connection, exec: &Execution) -> Result<()> {
    match &exec.goal_id {
        Some(goal) => scoped::require_same_owner_goal(conn, exec.owner, goal),
        None => Ok(()),
    }
}

fn insert_flow_row(conn: &Connection, flow: &Flow) -> Result<()> {
    conn.execute(
        "INSERT INTO flows (id, name, purpose, parent_flow, state, created_by, created_at, last_active, revision, owner_class, owner_uid)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            flow.id.as_str(),
            flow.name,
            flow.purpose,
            flow.parent_flow.as_ref().map(FlowId::as_str),
            flow.state.to_string(),
            flow.created_by,
            flow.created_at.as_millis(),
            flow.last_active.map(|t| t.as_millis()),
            flow.revision,
            flow.owner.class(),
            flow.owner.uid(),
        ],
    )?;
    Ok(())
}

fn insert_goal_row(conn: &Connection, goal: &Goal) -> Result<()> {
    conn.execute(
        "INSERT INTO goals (id, parent_goal, objective, rationale, constraints, success_metrics, priority, status, created_by, created_at, owner_class, owner_uid)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            goal.id.as_str(),
            goal.parent_goal.as_ref().map(GoalId::as_str),
            goal.objective,
            goal.rationale,
            serde_json::to_string(&goal.constraints)?,
            serde_json::to_string(&goal.success_metrics)?,
            goal.priority,
            goal.status.to_string(),
            goal.created_by,
            goal.created_at.as_millis(),
            goal.owner.class(),
            goal.owner.uid(),
        ],
    )?;
    Ok(())
}

fn insert_session_row(conn: &Connection, session: &Session) -> Result<()> {
    conn.execute(
        "INSERT INTO sessions (id, flow_id, kind, pid, cwd, state, presentation_state, created_by, created_at, owner_class, owner_uid)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            session.id.as_str(),
            session.flow_id.as_ref().map(FlowId::as_str),
            session.kind,
            session.pid,
            session.cwd,
            session.state.to_string(),
            session.presentation_state.to_string(),
            session.created_by,
            session.created_at.as_millis(),
            session.owner.class(),
            session.owner.uid(),
        ],
    )?;
    Ok(())
}

fn insert_execution_row(conn: &Connection, exec: &Execution) -> Result<()> {
    conn.execute(
        "INSERT INTO executions (id, agent_id, goal_id, reason, command, exit_code, duration_ms, started_at, owner_class, owner_uid)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            exec.id.as_str(),
            exec.agent_id.as_ref().map(AgentId::as_str),
            exec.goal_id.as_ref().map(GoalId::as_str),
            exec.reason,
            exec.command,
            exec.exit_code,
            exec.duration_ms,
            exec.started_at.as_millis(),
            exec.owner.class(),
            exec.owner.uid(),
        ],
    )?;
    Ok(())
}

fn insert_generation_row(conn: &Connection, generation: &Generation) -> Result<()> {
    conn.execute(
        "INSERT INTO generations (id, parent, source_commit, system_manifest, reason_goal, experiment, status, created_at, owner_class, owner_uid)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            generation.id.as_str(),
            generation.parent.as_ref().map(GenerationId::as_str),
            generation.source_commit,
            generation.system_manifest,
            generation.reason_goal.as_ref().map(GoalId::as_str),
            generation.experiment.as_ref().map(|e| e.to_string()),
            generation.status.to_string(),
            generation.created_at.as_millis(),
            generation.owner.class(),
            generation.owner.uid(),
        ],
    )?;
    Ok(())
}

fn append_profile_version_row(
    tx: &Connection,
    id: &ProfileId,
    toml: &str,
    summary: &str,
    appended_by: &str,
    edges: &[ProvenanceEdge],
) -> Result<u32> {
    let now = Timestamp::now();
    let next: u32 = tx
        .query_row(
            "SELECT COALESCE(MAX(version), 0) + 1
                 FROM ai_profile_versions WHERE profile_id = ?1",
            params![id.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(1);
    tx.execute(
        "INSERT INTO ai_profile_versions
                 (profile_id, version, toml, summary, created_by, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id.as_str(),
            next,
            toml,
            summary,
            appended_by,
            now.as_millis()
        ],
    )?;
    for edge in edges {
        insert_provenance_row(tx, edge)?;
    }
    Ok(next)
}

fn insert_provenance_row(conn: &Connection, edge: &ProvenanceEdge) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO provenance_edges (from_ref, relation, to_ref, created_at, note)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            edge.from.as_str(),
            edge.relation.as_str(),
            edge.to.as_str(),
            edge.created_at.as_millis(),
            edge.note,
        ],
    )?;
    Ok(())
}

fn insert_model_row(conn: &Connection, model: &ModelRecord) -> Result<()> {
    conn.execute(
        "INSERT INTO models (id, name, family, architecture, backend, context_tokens,
                             measured_memory_bytes, modalities, license, evidence,
                             created_by, created_at, retired_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            model.id.as_str(),
            model.name,
            model.family,
            model.architecture,
            model.backend.as_str(),
            model.context_tokens.map(|v| v as i64),
            model.measured_memory_bytes.map(|v| v as i64),
            serde_json::to_string(&model.modalities)?,
            model.license,
            model.evidence,
            model.created_by,
            model.created_at.as_millis(),
            model.retired_at.map(|t| t.as_millis()),
        ],
    )?;
    Ok(())
}

fn map_model(row: &Row<'_>) -> rusqlite::Result<ModelRecord> {
    let family: Option<String> = row.get(2)?;
    let architecture: Option<String> = row.get(3)?;
    let backend: String = row.get(4)?;
    let context_tokens: Option<i64> = row.get(5)?;
    let measured_memory_bytes: Option<i64> = row.get(6)?;
    let modalities_json: String = row.get(7)?;
    let modalities: Vec<String> = serde_json::from_str(&modalities_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(7, Type::Text, Box::new(error))
    })?;
    let license: Option<String> = row.get(8)?;
    let evidence: Option<String> = row.get(9)?;
    let created_at: i64 = row.get(11)?;
    let retired_at: Option<i64> = row.get(12)?;
    let map_u64 = |v: Option<i64>, column: usize| -> rusqlite::Result<Option<u64>> {
        match v {
            Some(value) if value < 0 => Err(rusqlite::Error::FromSqlConversionFailure(
                column,
                Type::Integer,
                Box::new(StorageError::InvalidOperation(
                    "negative catalog quantity in storage".into(),
                )),
            )),
            value => Ok(value.map(|raw| raw as u64)),
        }
    };
    Ok(ModelRecord {
        id: parse_text(row.get::<_, String>(0)?, 0)?,
        name: row.get(1)?,
        family,
        architecture,
        backend: parse_text(backend, 4)?,
        context_tokens: map_u64(context_tokens, 5)?,
        measured_memory_bytes: map_u64(measured_memory_bytes, 6)?,
        modalities,
        license,
        evidence,
        created_by: row.get(10)?,
        created_at: Timestamp::from_millis(created_at),
        retired_at: retired_at.map(Timestamp::from_millis),
    })
}

fn map_workload(row: &Row<'_>) -> rusqlite::Result<WorkloadRecord> {
    let ceilings: String = row.get(8)?;
    let ceilings = serde_json::from_str(&ceilings).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(8, Type::Text, Box::new(error))
    })?;
    let background: String = row.get(4)?;
    let state: String = row.get(6)?;
    let created_at: i64 = row.get(10)?;
    let admitted_at: Option<i64> = row.get(11)?;
    let stopped_at: Option<i64> = row.get(12)?;
    let completed_at: Option<i64> = row.get(13)?;
    Ok(WorkloadRecord {
        id: parse_text(row.get::<_, String>(0)?, 0)?,
        profile_id: parse_text(row.get::<_, String>(1)?, 1)?,
        profile_name: row.get(2)?,
        model_id: parse_text(row.get::<_, String>(3)?, 3)?,
        background: parse_workload_background(&background)?,
        priority: row.get(5)?,
        state: parse_text(state, 6)?,
        reason: row.get(7)?,
        ceilings,
        created_by: row.get(9)?,
        created_at: Timestamp::from_millis(created_at),
        admitted_at: admitted_at.map(Timestamp::from_millis),
        stopped_at: stopped_at.map(Timestamp::from_millis),
        completed_at: completed_at.map(Timestamp::from_millis),
        operation_id: row.get(14)?,
        owner: map_owner(row)?,
    })
}

/// Fetch the raw state string for a workload existence check.
fn load_workload_state(
    conn: &Connection,
    id: &WorkloadId,
) -> std::result::Result<Option<WorkloadState>, StorageError> {
    conn.query_row(
        "SELECT state FROM inference_workloads WHERE id = ?1",
        params![id.as_str()],
        |row| parse_text::<WorkloadState>(row.get::<_, String>(0)?, 0),
    )
    .optional()
    .map_err(StorageError::from)
}

fn parse_workload_background(raw: &str) -> rusqlite::Result<crate::profile::BackgroundActivity> {
    raw.parse().map_err(|error: String| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            Type::Text,
            Box::new(StorageError::InvalidOperation(error)),
        )
    })
}

fn parse_text<T>(raw: String, column: usize) -> rusqlite::Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    raw.parse().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(column, Type::Text, Box::new(error))
    })
}

fn map_provider(row: &Row<'_>) -> rusqlite::Result<ProviderRecord> {
    let endpoint: Option<String> = row.get(3)?;
    let capabilities_json: String = row.get(4)?;
    let capabilities: Vec<String> = serde_json::from_str(&capabilities_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(4, Type::Text, Box::new(error))
    })?;
    let created_at: i64 = row.get(6)?;
    let retired_at: Option<i64> = row.get(7)?;
    Ok(ProviderRecord {
        id: parse_text(row.get::<_, String>(0)?, 0)?,
        name: row.get(1)?,
        kind: row.get(2)?,
        endpoint,
        capabilities,
        created_by: row.get(5)?,
        created_at: Timestamp::from_millis(created_at),
        retired_at: retired_at.map(Timestamp::from_millis),
    })
}

/// Owner columns, read by name so positional mappers stay unchanged.
fn map_owner(row: &Row<'_>) -> rusqlite::Result<Owner> {
    let class: String = row.get("owner_class")?;
    let uid: Option<i64> = row.get("owner_uid")?;
    Ok(Owner::from_columns(&class, uid))
}

fn map_flow(row: &Row<'_>) -> rusqlite::Result<Flow> {
    let parent: Option<String> = row.get(3)?;
    let last_active: Option<i64> = row.get(7)?;
    let state: String = row.get(4)?;
    Ok(Flow {
        id: parse_text(row.get::<_, String>(0)?, 0)?,
        name: row.get(1)?,
        purpose: row.get(2)?,
        parent_flow: parent.map(|raw| parse_text(raw, 3)).transpose()?,
        state: parse_text(state, 4)?,
        created_by: row.get(5)?,
        created_at: Timestamp::from_millis(row.get(6)?),
        last_active: last_active.map(Timestamp::from_millis),
        revision: row.get(8)?,
        owner: map_owner(row)?,
    })
}

fn map_goal(row: &Row<'_>) -> rusqlite::Result<Goal> {
    let parent: Option<String> = row.get(1)?;
    let constraints: String = row.get(4)?;
    let metrics: String = row.get(5)?;
    let status: String = row.get(7)?;
    Ok(Goal {
        id: parse_text(row.get::<_, String>(0)?, 0)?,
        parent_goal: parent.map(|raw| parse_text(raw, 1)).transpose()?,
        objective: row.get(2)?,
        rationale: row.get(3)?,
        constraints: serde_json::from_str(&constraints).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(4, Type::Text, Box::new(error))
        })?,
        success_metrics: serde_json::from_str(&metrics).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(5, Type::Text, Box::new(error))
        })?,
        priority: row.get(6)?,
        status: parse_text(status, 7)?,
        created_by: row.get(8)?,
        created_at: Timestamp::from_millis(row.get(9)?),
        owner: map_owner(row)?,
    })
}

fn map_session(row: &Row<'_>) -> rusqlite::Result<Session> {
    let flow: Option<String> = row.get(1)?;
    let state: String = row.get(5)?;
    let presentation_state: String = row.get(6)?;
    Ok(Session {
        id: parse_text(row.get::<_, String>(0)?, 0)?,
        flow_id: flow.map(|raw| parse_text(raw, 1)).transpose()?,
        kind: row.get(2)?,
        pid: row.get(3)?,
        cwd: row.get(4)?,
        state: parse_text(state, 5)?,
        presentation_state: parse_text(presentation_state, 6)?,
        created_by: row.get(7)?,
        created_at: Timestamp::from_millis(row.get(8)?),
        owner: map_owner(row)?,
    })
}

fn map_execution(row: &Row<'_>) -> rusqlite::Result<Execution> {
    let agent: Option<String> = row.get(1)?;
    let goal: Option<String> = row.get(2)?;
    Ok(Execution {
        id: parse_text(row.get::<_, String>(0)?, 0)?,
        agent_id: agent.map(|raw| parse_text(raw, 1)).transpose()?,
        goal_id: goal.map(|raw| parse_text(raw, 2)).transpose()?,
        reason: row.get(3)?,
        command: row.get(4)?,
        exit_code: row.get(5)?,
        duration_ms: row.get(6)?,
        started_at: Timestamp::from_millis(row.get(7)?),
        owner: map_owner(row)?,
    })
}

fn map_generation(row: &Row<'_>) -> rusqlite::Result<Generation> {
    let parent: Option<String> = row.get(1)?;
    let goal: Option<String> = row.get(4)?;
    let experiment: Option<String> = row.get(5)?;
    let status: String = row.get(6)?;
    Ok(Generation {
        id: parse_text(row.get::<_, String>(0)?, 0)?,
        parent: parent.map(|raw| parse_text(raw, 1)).transpose()?,
        source_commit: row.get(2)?,
        system_manifest: row.get(3)?,
        reason_goal: goal.map(|raw| parse_text(raw, 4)).transpose()?,
        experiment: experiment.map(|raw| parse_text(raw, 5)).transpose()?,
        status: parse_text(status, 6)?,
        created_at: Timestamp::from_millis(row.get(7)?),
        owner: map_owner(row)?,
    })
}

fn map_provenance(row: &Row<'_>) -> rusqlite::Result<ProvenanceEdge> {
    let relation = parse_text(row.get::<_, String>(1)?, 1)?;
    let from = parse_text(row.get::<_, String>(0)?, 0)?;
    let to = parse_text(row.get::<_, String>(2)?, 2)?;
    Ok(ProvenanceEdge {
        from,
        relation,
        to,
        created_at: Timestamp::from_millis(row.get(3)?),
        note: row.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    /// A Flow row as written before migration 0009 (no owner columns).
    fn insert_legacy_flow(conn: &Connection, name: &str, state: &str, created_by: &str) {
        conn.execute(
            "INSERT INTO flows (id, name, state, created_by, created_at)
             VALUES (?1, ?2, ?3, ?4, 1)",
            params![FlowId::new().as_str(), name, state, created_by],
        )
        .unwrap();
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    use super::*;
    use crate::types::{Caller, ModelBackend, ProvenanceRelation};

    #[test]
    fn sqlite_extended_constraints_do_not_conflate_conflict_and_integrity() {
        let conn = Connection::open_in_memory().expect("open sqlite");
        conn.execute_batch(
            "PRAGMA foreign_keys=ON;
             CREATE TABLE parent (id INTEGER PRIMARY KEY);
             CREATE TABLE child (
                 id INTEGER PRIMARY KEY,
                 parent_id INTEGER NOT NULL REFERENCES parent(id),
                 value TEXT NOT NULL UNIQUE,
                 score INTEGER NOT NULL CHECK(score > 0)
             );
             INSERT INTO parent (id) VALUES (1);
             INSERT INTO child (id, parent_id, value, score) VALUES (1, 1, 'first', 1);",
        )
        .expect("create constrained tables");

        let cases = [
            (
                "INSERT INTO child (id, parent_id, value, score) VALUES (2, 1, 'first', 1)",
                rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE,
                true,
                false,
            ),
            (
                "INSERT INTO child (id, parent_id, value, score) VALUES (1, 1, 'second', 1)",
                rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY,
                true,
                false,
            ),
            (
                "INSERT INTO child (id, parent_id, value, score) VALUES (3, 999, 'third', 1)",
                rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY,
                false,
                true,
            ),
            (
                "INSERT INTO child (id, parent_id, value, score) VALUES (4, 1, 'fourth', 0)",
                rusqlite::ffi::SQLITE_CONSTRAINT_CHECK,
                false,
                false,
            ),
            (
                "INSERT INTO child (id, parent_id, value, score) VALUES (5, 1, NULL, 1)",
                rusqlite::ffi::SQLITE_CONSTRAINT_NOTNULL,
                false,
                false,
            ),
        ];
        for (sql, expected_extended, unique, foreign_key) in cases {
            let err = StorageError::from(conn.execute(sql, []).unwrap_err());
            let StorageError::Sqlite(rusqlite::Error::SqliteFailure(sqlite, _)) = &err else {
                panic!("expected SQLite constraint failure");
            };
            assert_eq!(sqlite.extended_code, expected_extended, "{sql}");
            assert_eq!(err.is_unique_violation(), unique, "{sql}");
            assert_eq!(err.is_foreign_key_violation(), foreign_key, "{sql}");
            assert!(!err.is_busy(), "{sql}");
        }
    }

    #[test]
    fn typed_storage_errors_are_not_inferred_from_error_messages() {
        let unique = StorageError::UniqueViolation("duplicate name".into());
        assert!(unique.is_unique_violation());
        assert!(!unique.is_foreign_key_violation());

        let foreign_key = StorageError::ForeignKeyViolation("missing parent".into());
        assert!(foreign_key.is_foreign_key_violation());
        assert!(!foreign_key.is_unique_violation());

        let corruption = StorageError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
            Some("UNIQUE constraint failed: misleading diagnostic".into()),
        ));
        assert!(!corruption.is_unique_violation());
        assert!(!corruption.is_foreign_key_violation());
        assert!(!corruption.is_busy());

        for code in [rusqlite::ffi::SQLITE_BUSY, rusqlite::ffi::SQLITE_LOCKED] {
            let busy = StorageError::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(code),
                None,
            ));
            assert!(busy.is_busy());
            assert!(!busy.is_unique_violation());
            assert!(!busy.is_foreign_key_violation());
        }

        let generic_constraint = StorageError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            None,
        ));
        assert!(!generic_constraint.is_unique_violation());
        assert!(!generic_constraint.is_foreign_key_violation());
    }

    fn sqlite_code_error(code: std::os::raw::c_int, message: Option<&str>) -> StorageError {
        StorageError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(code),
            message.map(str::to_owned),
        ))
    }

    #[test]
    fn ipc_classification_covers_every_storage_error_family() {
        use crate::ipc::ErrorCode;
        let cases = [
            (StorageError::NotFound("x".into()), ErrorCode::NotFound),
            (
                StorageError::PermissionDenied("x".into()),
                ErrorCode::PermissionDenied,
            ),
            (
                StorageError::InvalidOperation("x".into()),
                ErrorCode::InvalidArgument,
            ),
            (
                StorageError::ForeignKeyViolation("x".into()),
                ErrorCode::InvalidArgument,
            ),
            (
                StorageError::OperationConflict("x".into()),
                ErrorCode::Conflict,
            ),
            (
                StorageError::UniqueViolation("x".into()),
                ErrorCode::Conflict,
            ),
            (
                StorageError::Serde(serde_json::from_str::<u8>("{").unwrap_err()),
                ErrorCode::Internal,
            ),
            (
                StorageError::Io(std::io::Error::other("disk")),
                ErrorCode::Internal,
            ),
            (
                StorageError::Sqlite(rusqlite::Error::QueryReturnedNoRows),
                ErrorCode::Internal,
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.ipc_error_code(), expected, "{error}");
        }
    }

    #[test]
    fn ipc_classification_uses_sqlite_codes_not_message_text() {
        use crate::ipc::ErrorCode;
        let transient = [
            rusqlite::ffi::SQLITE_BUSY,
            rusqlite::ffi::SQLITE_BUSY_SNAPSHOT,
            rusqlite::ffi::SQLITE_BUSY_RECOVERY,
            rusqlite::ffi::SQLITE_BUSY_TIMEOUT,
            rusqlite::ffi::SQLITE_LOCKED,
            rusqlite::ffi::SQLITE_LOCKED_SHAREDCACHE,
        ];
        for code in transient {
            assert_eq!(
                sqlite_code_error(code, None).ipc_error_code(),
                ErrorCode::Unavailable,
                "code {code}"
            );
        }

        // Damage, defects and environment failures are never retryable,
        // even when the diagnostic text mimics a transient or typed error.
        let permanent = [
            rusqlite::ffi::SQLITE_CORRUPT,
            rusqlite::ffi::SQLITE_NOTADB,
            rusqlite::ffi::SQLITE_READONLY,
            rusqlite::ffi::SQLITE_FULL,
            rusqlite::ffi::SQLITE_IOERR,
            rusqlite::ffi::SQLITE_CANTOPEN,
            rusqlite::ffi::SQLITE_MISMATCH,
            rusqlite::ffi::SQLITE_CONSTRAINT,
            rusqlite::ffi::SQLITE_CONSTRAINT_CHECK,
            rusqlite::ffi::SQLITE_CONSTRAINT_NOTNULL,
            rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER,
        ];
        for code in permanent {
            assert_eq!(
                sqlite_code_error(code, Some("database is locked; UNIQUE constraint failed"))
                    .ipc_error_code(),
                ErrorCode::Internal,
                "code {code}"
            );
        }

        assert_eq!(
            sqlite_code_error(rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE, None).ipc_error_code(),
            ErrorCode::Conflict
        );
        assert_eq!(
            sqlite_code_error(rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY, None).ipc_error_code(),
            ErrorCode::Conflict
        );
        assert_eq!(
            sqlite_code_error(rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY, None).ipc_error_code(),
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            StorageError::NotFound("database is locked".into()).ipc_error_code(),
            ErrorCode::NotFound
        );
    }

    #[test]
    fn ipc_classification_of_real_sqlite_failures() {
        use crate::ipc::ErrorCode;
        let dir = tempfile::tempdir().expect("tempdir");

        // A second connection holding the writer lock yields a real BUSY.
        let db = dir.path().join("contended.db");
        let holder = Connection::open(&db).expect("open holder");
        holder
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE t (id INTEGER PRIMARY KEY);
                 BEGIN IMMEDIATE;
                 INSERT INTO t (id) VALUES (1);",
            )
            .expect("hold writer lock");
        let contender = Connection::open(&db).expect("open contender");
        contender
            .busy_timeout(std::time::Duration::ZERO)
            .expect("zero busy timeout");
        let busy = StorageError::from(
            contender
                .execute("INSERT INTO t (id) VALUES (2)", [])
                .unwrap_err(),
        );
        assert_eq!(busy.ipc_error_code(), ErrorCode::Unavailable, "{busy}");
        holder.execute_batch("COMMIT;").expect("release lock");
        contender
            .execute("INSERT INTO t (id) VALUES (2)", [])
            .expect("write recovers after lock release");

        // A file that is not a database must not look transient.
        let garbage = dir.path().join("garbage.db");
        std::fs::write(&garbage, vec![0x5a_u8; 8192]).expect("write garbage");
        let error = Store::open(&garbage)
            .and_then(|mut store| store.migrate().map(|_| ()))
            .expect_err("garbage file must not open as a database");
        assert_eq!(error.ipc_error_code(), ErrorCode::Internal, "{error}");

        // Real constraint failures through a migrated-style connection.
        let conn = Connection::open_in_memory().expect("open sqlite");
        conn.execute_batch(
            "PRAGMA foreign_keys=ON;
             CREATE TABLE parent (id INTEGER PRIMARY KEY);
             CREATE TABLE child (
                 id INTEGER PRIMARY KEY,
                 parent_id INTEGER NOT NULL REFERENCES parent(id),
                 score INTEGER NOT NULL CHECK(score > 0)
             );
             INSERT INTO parent (id) VALUES (1);
             INSERT INTO child (id, parent_id, score) VALUES (1, 1, 1);",
        )
        .expect("create tables");
        let classify = |sql: &str| StorageError::from(conn.execute(sql, []).unwrap_err());
        assert_eq!(
            classify("INSERT INTO child (id, parent_id, score) VALUES (1, 1, 1)").ipc_error_code(),
            ErrorCode::Conflict
        );
        assert_eq!(
            classify("INSERT INTO child (id, parent_id, score) VALUES (2, 999, 1)")
                .ipc_error_code(),
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            classify("INSERT INTO child (id, parent_id, score) VALUES (3, 1, 0)").ipc_error_code(),
            ErrorCode::Internal
        );
    }

    fn migrated_store() -> Store {
        let mut store = Store::open_in_memory().expect("open store");
        store.migrate().expect("migrate");
        store
    }

    #[test]
    fn status_counts_match_domain_listings() {
        let mut store = migrated_store();
        assert_eq!(
            store.status_counts().expect("empty counts"),
            StatusCounts::default()
        );

        store.ensure_general_flow().expect("general flow");
        store
            .insert_goal(&Goal::new("count me", "test"))
            .expect("insert goal");
        let kept = example_model();
        let mut retired = example_model();
        retired.name = "retired-model".into();
        store
            .insert_model_with_provenance(&kept, &[])
            .expect("insert model");
        store
            .insert_model_with_provenance(&retired, &[])
            .expect("insert retired model");
        store
            .retire_model(&retired.id, "uid:1000")
            .expect("retire model");

        let counts = store.status_counts().expect("counts");
        assert_eq!(counts.flows, store.list_flows().unwrap().len() as u64);
        assert_eq!(counts.goals, store.list_goals().unwrap().len() as u64);
        assert_eq!(counts.models, store.list_models().unwrap().len() as u64);
        assert_eq!(counts.models, 1, "retired models are not counted");
        assert_eq!(counts.sessions, store.list_sessions().unwrap().len() as u64);
        assert_eq!(
            counts.providers,
            store.list_providers().unwrap().len() as u64
        );
        assert_eq!(counts.flows, 1);
        assert_eq!(counts.goals, 1);
    }

    #[test]
    fn status_counts_fail_on_damaged_schema() {
        let store = migrated_store();
        store
            .conn
            .execute_batch("DROP TABLE provenance_edges;")
            .expect("drop table");
        let error = store
            .status_counts()
            .expect_err("missing table must not report counts");
        assert_eq!(error.ipc_error_code(), crate::ipc::ErrorCode::Internal);
    }

    fn example_model() -> ModelRecord {
        let mut model = ModelRecord::new("qwen2-coder-14b", "uid:1000");
        model.family = Some("qwen2".into());
        model.architecture = Some("transformer".into());
        model.backend = ModelBackend::Cpu;
        model.context_tokens = Some(32_768);
        model.measured_memory_bytes = Some(9_000_000_000);
        model.modalities = vec!["text".into()];
        model.license = Some("apache-2.0".into());
        model.evidence = Some("measure:gold-rss".into());
        model
    }

    fn model_operation<'a>(model: &'a ModelRecord, replay: &'a str) -> MutationOperation<'a> {
        MutationOperation {
            actor: model.created_by.as_str(),
            command: "model.register",
            operation_id: replay,
            request_payload: "static",
            response_payload: "static",
        }
    }

    #[test]
    fn model_registration_is_idempotent_and_durable() {
        let mut store = migrated_store();
        let model = example_model();
        let edge = ProvenanceEdge {
            from: ObjectRef::from(&model.id),
            relation: ProvenanceRelation::CreatedBy,
            to: ObjectRef::actor(model.created_by.clone()).unwrap(),
            created_at: Timestamp::now(),
            note: Some("registered by profile test".into()),
        };
        let operation = model_operation(&model, "replay-1");
        let first = store
            .insert_model_with_provenance_once(&model, std::slice::from_ref(&edge), &operation)
            .unwrap();
        let replayed = store
            .insert_model_with_provenance_once(&model, &[edge], &operation)
            .unwrap();
        assert_eq!(first, replayed);
        assert_eq!(store.list_models().unwrap().len(), 1);
        let stored = store.get_model(&model.id).unwrap().unwrap();
        assert_eq!(stored.name, "qwen2-coder-14b");
        assert_eq!(stored.backend, ModelBackend::Cpu);
        assert_eq!(stored.modalities, vec!["text".to_owned()]);
        assert!(stored.retired_at.is_none());
    }

    #[test]
    fn model_operation_replay_with_changed_payload_conflicts() {
        let mut store = migrated_store();
        let model = example_model();
        let operation = model_operation(&model, "replay-2");
        store
            .insert_model_with_provenance_once(&model, &[], &operation)
            .unwrap();
        let mut conflicting = model_operation(&model, "replay-2");
        conflicting.request_payload = "changed";
        let result = store.insert_model_with_provenance_once(&model, &[], &conflicting);
        assert!(matches!(result, Err(StorageError::OperationConflict(_))));
    }

    #[test]
    fn model_retirement_is_soft_and_typed() {
        let mut store = migrated_store();
        let model = example_model();
        let operation = model_operation(&model, "replay-3");
        store
            .insert_model_with_provenance_once(&model, &[], &operation)
            .unwrap();
        let missing = store.retire_model(&ModelId::from_raw("model-missing"), "uid:1000");
        assert!(matches!(missing, Err(StorageError::NotFound(_))));
        store.retire_model(&model.id, "uid:1000").unwrap();
        let retired = store.get_model(&model.id).unwrap().unwrap();
        assert!(retired.is_retired());
        assert!(
            store.list_models().unwrap().is_empty(),
            "retired rows leave the active list"
        );
        store.retire_model(&model.id, "uid:1000").unwrap();
    }

    #[test]
    fn profile_create_replays_and_rejects_name_conflicts() {
        let mut store = migrated_store();
        let first = store
            .create_profile(
                &ProfileId::from_raw("profile-static"),
                "expert",
                "uid:1000",
                "capability_tier = 'advanced'\n",
                "{}",
                Owner::User(1000),
                Some(MutationOperation {
                    actor: "uid:1000",
                    command: "profile.create",
                    operation_id: "op-profile-1",
                    request_payload: "static",
                    response_payload: "{\"id\":\"profile-static\",\"version\":1}",
                }),
                &[],
            )
            .unwrap();
        let replay = store
            .create_profile(
                &ProfileId::from_raw("profile-static-replay-must-not-insert"),
                "expert",
                "uid:1000",
                "capability_tier = 'advanced'\n",
                "{}",
                Owner::User(1000),
                Some(MutationOperation {
                    actor: "uid:1000",
                    command: "profile.create",
                    operation_id: "op-profile-1",
                    request_payload: "static",
                    response_payload: "static",
                }),
                &[],
            )
            .unwrap();
        assert_eq!(first, replay);
        assert!(replay.contains("profile-static"));
        let duplicate = store.create_profile(
            &ProfileId::new(),
            "expert",
            "uid:1000",
            "fallback = 'queue'\n",
            "{}",
            Owner::User(1000),
            None,
            &[],
        );
        assert!(duplicate.unwrap_err().is_unique_violation());
    }

    #[test]
    fn profile_versions_are_append_only_and_rename_is_typed() {
        let mut store = migrated_store();
        let id = ProfileId::new();
        store
            .create_profile(
                &id,
                "base",
                "uid:1000",
                "fallback = 'ask'\n",
                "{}",
                Owner::User(1000),
                None,
                &[],
            )
            .unwrap();
        let owner = Caller::new(1000);
        let v2 = store
            .append_profile_version_for(owner, &id, "fallback = 'queue'\n", "{}", "uid:1000", &[])
            .unwrap();
        assert_eq!(v2, 2);
        let (latest, latest_toml, _) = store.profile_latest_for(owner, &id).unwrap().unwrap();
        assert_eq!(latest, 2);
        assert!(latest_toml.contains("queue"));
        store
            .rename_profile_for(owner, &id, "renamed", "uid:1000")
            .unwrap();
        let view = store.profile_for(owner, &id).unwrap().unwrap();
        assert_eq!(view.name, "renamed");
        let missing =
            store.rename_profile_for(owner, &ProfileId::from_raw("profile-none"), "x", "uid:1000");
        assert!(matches!(missing, Err(StorageError::NotFound(_))));
        let by_name = store
            .profile_base_for(Owner::User(1000), "renamed")
            .unwrap()
            .unwrap();
        assert_eq!(by_name.id, id);
        assert_eq!(store.profiles_for(owner).unwrap().len(), 1);
    }

    #[test]
    fn provider_registration_is_portfolio_only() {
        let mut store = migrated_store();
        store
            .register_provider(
                &ProviderId::new(),
                "local-echo",
                "local-unix",
                None,
                &["text".to_owned()],
                "uid:1000",
                None,
                &[],
            )
            .unwrap();
        let rows = store.list_providers().unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].endpoint.is_none());
        assert_eq!(rows[0].capabilities, vec!["text".to_owned()]);
    }

    #[test]
    fn migration_is_idempotent() {
        let mut store = Store::open_in_memory().expect("open store");
        let first = store.migrate().expect("first migrate");
        assert_eq!(first.len(), MIGRATIONS.len());
        let second = store.migrate().expect("second migrate");
        assert!(second.is_empty(), "second migrate should be a no-op");
        assert_eq!(store.applied_migrations().unwrap().len(), MIGRATIONS.len());
    }

    /// A database as installed before ownership: migrations 1..=8 applied.
    fn pre_ownership_store() -> Store {
        let store = Store::open_in_memory().expect("open store");
        store
            .conn
            .execute_batch(
                "CREATE TABLE schema_migrations (
                     version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at INTEGER NOT NULL);",
            )
            .unwrap();
        for migration in &MIGRATIONS[..8] {
            store.conn.execute_batch(migration.sql).unwrap();
            store
                .conn
                .execute(
                    "INSERT INTO schema_migrations VALUES (?1, ?2, 1)",
                    params![migration.version, migration.name],
                )
                .unwrap();
        }
        store
    }

    fn owner_of(conn: &Connection, table: &str, id: &str) -> (String, Option<i64>) {
        conn.query_row(
            &format!("SELECT owner_class, owner_uid FROM {table} WHERE id = ?1"),
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    }

    /// #225: the backfill keeps only well-formed `uid:<n>` creators, marks
    /// General as system, and leaves every legacy or malformed label
    /// unresolved. Nothing is reassigned to another uid.
    #[test]
    fn ownership_backfill_is_conservative() {
        let mut store = pre_ownership_store();
        let c = &store.conn;
        let flow = |id: &str, name: &str, created_by: &str| {
            c.execute(
                "INSERT INTO flows (id, name, state, created_by, created_at)
                 VALUES (?1, ?2, 'ACTIVE', ?3, 1)",
                params![id, name, created_by],
            )
            .unwrap();
        };
        flow("flow-general", "General", "system");
        flow("flow-alice", "alice", "uid:998");
        flow("flow-root", "root", "uid:0");
        flow("flow-legacy", "legacy", "user");
        for (i, bad) in [
            "uid:",
            "uid:12abc",
            "uid:-1",
            "uid: 7",
            "uid:99999999999",
            "uid:4294967295",
            "UID:5",
            "uid:5\n",
            "agent:planner",
            "",
        ]
        .iter()
        .enumerate()
        {
            flow(&format!("flow-bad{i}"), &format!("bad{i}"), bad);
        }
        c.execute(
            "INSERT INTO goals (id, objective, status, created_by, created_at)
             VALUES ('goal-a', 'o', 'ACTIVE', 'uid:998', 1), ('goal-l', 'o', 'ACTIVE', 'user', 1)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO executions (id, goal_id, started_at) VALUES
             ('exec-a', 'goal-a', 1), ('exec-l', 'goal-l', 1), ('exec-none', NULL, 1)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO sessions (id, kind, state, created_by, created_at)
             VALUES ('session-a', 'k', 'RUNNING', 'uid:998', 1),
                    ('session-l', 'k', 'RUNNING', 'user', 1)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO generations (id, status, created_at) VALUES
             ('gen-cand', 'CANDIDATE', 1), ('gen-test', 'TESTED', 1),
             ('gen-rej', 'REJECTED', 1), ('gen-stable', 'STABLE', 1),
             ('gen-old', 'SUPERSEDED', 1)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO ai_providers (id, name, kind, capabilities, created_by, created_at)
             VALUES ('provider-1', 'p', 'local-unix', '[]', 'uid:998', 1)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO ai_profiles (id, name, created_by, created_at)
             VALUES ('profile-a', 'a', 'uid:998', 1), ('profile-l', 'l', 'user', 1)",
            [],
        )
        .unwrap();

        let applied = store.migrate().unwrap();
        assert_eq!(
            applied.last().map(|m| m.version),
            MIGRATIONS.last().map(|m| m.version)
        );
        let c = &store.conn;
        let user = |uid| ("user".to_owned(), Some(uid));
        let unresolved = ("unresolved".to_owned(), None);
        assert_eq!(
            owner_of(c, "flows", "flow-general"),
            ("system".to_owned(), None)
        );
        assert_eq!(owner_of(c, "flows", "flow-alice"), user(998));
        assert_eq!(owner_of(c, "flows", "flow-root"), user(0));
        assert_eq!(owner_of(c, "flows", "flow-legacy"), unresolved);
        for i in 0..10 {
            assert_eq!(
                owner_of(c, "flows", &format!("flow-bad{i}")),
                unresolved,
                "bad{i}"
            );
        }
        assert_eq!(owner_of(c, "goals", "goal-a"), user(998));
        assert_eq!(owner_of(c, "goals", "goal-l"), unresolved);
        assert_eq!(owner_of(c, "executions", "exec-a"), user(998));
        assert_eq!(owner_of(c, "executions", "exec-l"), unresolved);
        assert_eq!(owner_of(c, "executions", "exec-none"), unresolved);
        assert_eq!(owner_of(c, "sessions", "session-a"), user(998));
        assert_eq!(owner_of(c, "sessions", "session-l"), unresolved);
        // Existing candidates are never silently published as system records.
        for id in ["gen-cand", "gen-test", "gen-rej"] {
            assert_eq!(owner_of(c, "generations", id), unresolved, "{id}");
        }
        for id in ["gen-stable", "gen-old"] {
            assert_eq!(
                owner_of(c, "generations", id),
                ("system".to_owned(), None),
                "{id}"
            );
        }
        assert_eq!(
            owner_of(c, "ai_providers", "provider-1"),
            ("system".to_owned(), None)
        );
        assert_eq!(owner_of(c, "ai_profiles", "profile-a"), user(998));
        assert_eq!(owner_of(c, "ai_profiles", "profile-l"), unresolved);
        // Creator audit text is untouched, and no row lost its identity.
        let created_by: String = c
            .query_row(
                "SELECT created_by FROM flows WHERE id = 'flow-legacy'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(created_by, "user");
        assert_eq!(count(c, "SELECT COUNT(*) FROM flows"), 14);
    }

    /// A write path that forgets the owner fails closed, and the owner
    /// columns reject inconsistent combinations at the SQLite boundary.
    #[test]
    fn owner_columns_fail_closed_and_reject_inconsistent_values() {
        let store = migrated_store();
        let c = &store.conn;
        c.execute(
            "INSERT INTO goals (id, objective, status, created_by, created_at)
             VALUES ('goal-x', 'o', 'ACTIVE', 'uid:5', 1)",
            [],
        )
        .unwrap();
        assert_eq!(
            owner_of(c, "goals", "goal-x"),
            ("unresolved".to_owned(), None)
        );
        for (class, column_value) in [
            ("user", "NULL"),
            ("system", "5"),
            ("unresolved", "5"),
            ("admin", "NULL"),
            ("user", "-1"),
            ("user", "4294967295"),
        ] {
            let result = c.execute(
                &format!(
                    "INSERT INTO goals (id, objective, status, created_by, created_at,
                                        owner_class, owner_uid)
                     VALUES ('goal-{class}{column_value}', 'o', 'ACTIVE', 'x', 1, '{class}', {column_value})"
                ),
                [],
            );
            assert!(result.is_err(), "{class} {column_value} must be rejected");
        }
    }

    /// An interrupted migration leaves no partial state: a failure in the
    /// middle of 0009 rolls back every earlier ALTER and its version record.
    #[test]
    fn failed_ownership_migration_leaves_no_partial_state() {
        let mut store = pre_ownership_store();
        store
            .conn
            .execute(
                "INSERT INTO goals (id, objective, status, created_by, created_at)
                 VALUES ('goal-a', 'o', 'ACTIVE', 'uid:998', 1)",
                [],
            )
            .unwrap();
        // Sabotage the middle of the migration: `sessions` already has the
        // column, so ALTER TABLE sessions ADD COLUMN owner_class fails after
        // flows and goals were altered.
        store
            .conn
            .execute_batch("ALTER TABLE sessions ADD COLUMN owner_class TEXT;")
            .unwrap();
        assert!(store.migrate().is_err());
        let c = &store.conn;
        assert_eq!(
            count(
                c,
                "SELECT COUNT(*) FROM schema_migrations WHERE version = 9"
            ),
            0
        );
        for table in ["flows", "goals"] {
            let columns = count(
                c,
                &format!(
                    "SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name LIKE 'owner%'"
                ),
            );
            assert_eq!(columns, 0, "{table} kept a partial ownership schema");
        }
        assert_eq!(count(c, "SELECT COUNT(*) FROM goals"), 1);
        // Repairing the cause lets the same database migrate cleanly later.
        store
            .conn
            .execute_batch("ALTER TABLE sessions DROP COLUMN owner_class;")
            .unwrap();
        store.migrate().unwrap();
        assert_eq!(
            owner_of(&store.conn, "goals", "goal-a"),
            ("user".to_owned(), Some(998))
        );
    }

    /// Restart: ownership survives reopening the file, and re-running the
    /// migrations neither duplicates nor changes it.
    #[test]
    fn ownership_survives_restart_and_repeated_migration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let goal = {
            let mut store = Store::open(&path).unwrap();
            store.migrate().unwrap();
            store.ensure_general_flow().unwrap();
            let mut goal = Goal::new("owned", "uid:998");
            goal.owner = Owner::User(998);
            store.insert_goal(&goal).unwrap();
            goal
        };
        let mut store = Store::open(&path).unwrap();
        assert!(store.migrate().unwrap().is_empty());
        assert_eq!(
            store.get_goal(&goal.id).unwrap().unwrap().owner,
            Owner::User(998)
        );
        let general_id = store.ensure_general_flow().unwrap();
        let general = store.get_flow(&general_id).unwrap().unwrap();
        assert_eq!(general.owner, Owner::System);
    }

    #[test]
    fn migration_two_normalizes_legacy_session_and_actor_state() {
        let mut store = Store::open_in_memory().expect("open store");
        store
            .conn
            .execute_batch(
                "CREATE TABLE schema_migrations (
                     version INTEGER PRIMARY KEY,
                     name TEXT NOT NULL,
                     applied_at INTEGER NOT NULL
                 );",
            )
            .expect("create migration table");
        store
            .conn
            .execute_batch(MIGRATIONS[0].sql)
            .expect("apply legacy schema");
        store
            .conn
            .execute(
                "INSERT INTO schema_migrations (version, name, applied_at)
                 VALUES (1, 'init', 1)",
                [],
            )
            .expect("record legacy migration");

        let session_id = SessionId::new();
        store
            .conn
            .execute(
                "INSERT INTO sessions
                 (id, kind, state, created_by, created_at)
                 VALUES (?1, 'local-pty', 'DETACHED', 'user', 1)",
                params![session_id.as_str()],
            )
            .expect("insert legacy session");

        let from_ref = session_id.to_string();
        store
            .conn
            .execute(
                "INSERT INTO provenance_edges
                 (from_ref, relation, to_ref, created_at)
                 VALUES (?1, 'created_by', 'user', 1)",
                params![from_ref],
            )
            .expect("insert legacy actor edge");
        store
            .conn
            .execute(
                "INSERT INTO provenance_edges
                 (from_ref, relation, to_ref, created_at)
                 VALUES (?1, 'created_by', 'actor:user', 1)",
                params![session_id.as_str()],
            )
            .expect("insert already-normalized duplicate");

        let applied = store.migrate().expect("apply remaining migrations");
        assert_eq!(applied.len(), MIGRATIONS.len() - 1);
        assert_eq!(applied[0].version, 2);
        assert_eq!(applied[1].version, 3);
        assert_eq!(applied[2].version, 4);
        assert_eq!(applied[3].version, 5);
        assert_eq!(applied[4].version, 6);
        assert_eq!(applied[5].version, 7);
        assert_eq!(applied[6].version, 8);

        let session = store
            .get_session(&session_id)
            .expect("read migrated session")
            .expect("session exists");
        assert_eq!(session.state, SessionState::Running);
        assert_eq!(
            session.presentation_state,
            SessionPresentationState::Detached
        );

        let edges = store
            .provenance_from(&ObjectRef::from(&session_id))
            .expect("read migrated provenance");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to.as_str(), "actor:user");
    }

    #[test]
    fn file_backed_stores_share_the_same_state() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state.db");

        let mut writer = Store::open(&path).expect("open writer");
        writer.migrate().expect("migrate writer");
        let mut reader = Store::open(&path).expect("open reader");
        reader.migrate().expect("migrate reader");

        let flow = Flow::new("Shared", "test");
        writer.insert_flow(&flow).expect("insert flow");

        let loaded = reader
            .get_flow(&flow.id)
            .expect("read flow")
            .expect("flow exists");
        assert_eq!(loaded.id, flow.id);
    }

    #[test]
    fn concurrent_migrators_converge_on_one_schema() {
        use std::sync::{Arc, Barrier};

        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state.db");
        let barrier = Arc::new(Barrier::new(2));

        let handles: Vec<_> = (0..2)
            .map(|_| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let mut store = Store::open(path).expect("open store");
                    barrier.wait();
                    store.migrate().expect("concurrent migration");
                    store.applied_migrations().expect("migration list")
                })
            })
            .collect();

        for handle in handles {
            let migrations = handle.join().expect("migration thread");
            assert_eq!(migrations.len(), MIGRATIONS.len());
        }
    }

    #[test]
    fn foreground_is_not_a_flow_state_after_0010() {
        let store = migrated_store();
        let mut flow = Flow::new("A", "uid:1000");
        flow.owner = Owner::User(1000);
        store.insert_flow(&flow).unwrap();
        assert!(store
            .conn
            .execute(
                "UPDATE flows SET state = 'FOREGROUND' WHERE id = ?1",
                params![flow.id.as_str()],
            )
            .is_err());
        assert!(store
            .conn
            .execute(
                "INSERT INTO flows (id, name, state, created_by, created_at)
                 VALUES ('flow-x', 'X', 'FOREGROUND', 'x', 1)",
                [],
            )
            .is_err());
        assert_eq!(
            count(
                &store.conn,
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'idx_flows_single_foreground'"
            ),
            0
        );
    }

    #[test]
    fn migration_rejects_legacy_multiple_foregrounds_without_data_loss() {
        let store = Store::open_in_memory().unwrap();
        store.conn.execute_batch(MIGRATIONS[0].sql).unwrap();
        store.conn.execute_batch(MIGRATIONS[1].sql).unwrap();
        // Pre-0009 rows have no owner columns.
        for name in ["A", "B"] {
            insert_legacy_flow(&store.conn, name, "FOREGROUND", "test");
        }
        assert!(store.conn.execute_batch(MIGRATIONS[2].sql).is_err());
        assert_eq!(
            count(
                &store.conn,
                "SELECT COUNT(*) FROM flows WHERE state = 'FOREGROUND'"
            ),
            2,
            "failed index migration must not silently change legacy Flow state"
        );
    }

    #[test]
    fn general_bootstrap_is_stable_across_restart_and_connections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let caller = crate::types::Caller::new(1000);
        let original = {
            let mut store = Store::open(&path).unwrap();
            store.migrate().unwrap();
            let id = store.ensure_general_flow().unwrap();
            assert_eq!(id.as_str(), "flow-general");
            let general = store.get_flow(&id).unwrap().unwrap();
            assert_eq!(general.state, FlowState::Active);
            assert_eq!(general.owner, Owner::System);
            assert_eq!(store.foreground_flow_for(caller).unwrap().flow.id, id);
            id
        };
        let mut second = Store::open(&path).unwrap();
        second.migrate().unwrap();
        assert_eq!(second.ensure_general_flow().unwrap(), original);
        assert_eq!(
            second
                .list_flows()
                .unwrap()
                .iter()
                .filter(|f| f.name == "General")
                .count(),
            1
        );
        let mut other = Flow::new("Other", "uid:1000");
        other.owner = Owner::User(1000);
        second.insert_flow(&other).unwrap();
        second.activate_flow_for(caller, &other.id).unwrap();
        // Restart bootstrap never resets anyone's foreground.
        assert_eq!(second.ensure_general_flow().unwrap(), original);
        assert_eq!(
            second.foreground_flow_for(caller).unwrap().flow.id,
            other.id
        );
    }

    #[test]
    fn existing_legacy_general_id_is_preserved_and_archived_state_recovers() {
        let mut store = migrated_store();
        let mut general = Flow::new("General", "system");
        general.state = FlowState::Archived;
        let legacy_id = general.id.clone();
        store.insert_flow(&general).unwrap();
        assert_eq!(store.ensure_general_flow().unwrap(), legacy_id);
        let recovered = store.get_flow(&legacy_id).unwrap().unwrap();
        assert_eq!(recovered.state, FlowState::Active);
        // The canonical General is always the shared system Flow.
        assert_eq!(recovered.owner, Owner::System);
    }

    #[test]
    fn general_identity_is_protected_against_sql_writers() {
        let mut store = migrated_store();
        let id = store.ensure_general_flow().unwrap();
        assert!(store.insert_flow(&Flow::new("General", "user")).is_err());
        assert!(store.set_flow_state(&id, FlowState::Archived).is_err());
        assert!(store
            .conn
            .execute("DELETE FROM flows WHERE id = ?1", params![id.as_str()])
            .is_err());
        assert!(store
            .conn
            .execute(
                "UPDATE flows SET name = 'Renamed' WHERE id = ?1",
                params![id.as_str()],
            )
            .is_err());
    }

    #[test]
    fn duplicate_legacy_general_rows_fail_migration_without_mutation() {
        let store = Store::open_in_memory().unwrap();
        store.conn.execute_batch(MIGRATIONS[0].sql).unwrap();
        store.conn.execute_batch(MIGRATIONS[1].sql).unwrap();
        store.conn.execute_batch(MIGRATIONS[2].sql).unwrap();
        insert_legacy_flow(&store.conn, "General", "ACTIVE", "system");
        insert_legacy_flow(&store.conn, "General", "ACTIVE", "system");
        assert!(store.conn.execute_batch(MIGRATIONS[3].sql).is_err());
        assert_eq!(count(&store.conn, "SELECT COUNT(*) FROM flows"), 2);
    }

    #[test]
    fn flow_persists_and_round_trips() {
        let store = migrated_store();
        let flow = Flow::new("Development", "user");
        store.insert_flow(&flow).expect("insert flow");
        let loaded = store.get_flow(&flow.id).expect("get flow").expect("exists");
        assert_eq!(flow, loaded);
        assert_eq!(store.list_flows().unwrap().len(), 1);
    }

    #[test]
    fn goal_and_provenance_round_trip() {
        let store = migrated_store();
        let goal = Goal::new("Reduce build latency", "agent:planner");
        store.insert_goal(&goal).expect("insert goal");

        let flow = Flow::new("Development", "user");
        store.insert_flow(&flow).expect("insert flow");

        let edge = ProvenanceEdge {
            from: ObjectRef::from(&goal.id),
            relation: ProvenanceRelation::BelongsTo,
            to: ObjectRef::from(&flow.id),
            created_at: Timestamp::now(),
            note: None,
        };
        store.insert_provenance(&edge).expect("insert edge");

        store.insert_provenance(&edge).expect("insert edge again");

        let edges = store
            .provenance_from(&ObjectRef::from(&goal.id))
            .expect("query edges");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].relation, ProvenanceRelation::BelongsTo);
    }

    #[test]
    fn object_and_provenance_can_commit_atomically() {
        let mut store = migrated_store();
        let flow = Flow::new("Atomic", "user");
        let edge = ProvenanceEdge {
            from: ObjectRef::from(&flow.id),
            relation: ProvenanceRelation::CreatedBy,
            to: ObjectRef::actor("user").expect("actor ref"),
            created_at: Timestamp::now(),
            note: None,
        };

        store
            .insert_flow_with_provenance(&flow, &[edge])
            .expect("atomic insert");

        assert!(store.get_flow(&flow.id).expect("flow query").is_some());
        assert_eq!(
            store
                .provenance_from(&ObjectRef::from(&flow.id))
                .expect("provenance query")
                .len(),
            1
        );
    }

    #[test]
    fn session_runtime_and_presentation_round_trip_independently() {
        let store = migrated_store();
        let session = Session {
            owner: crate::types::Owner::User(1000),
            id: SessionId::new(),
            flow_id: None,
            kind: "local-pty".to_owned(),
            pid: Some(42),
            cwd: Some("/tmp".to_owned()),
            state: SessionState::Running,
            presentation_state: SessionPresentationState::Detached,
            created_by: "test".to_owned(),
            created_at: Timestamp::now(),
        };
        store.insert_session(&session).expect("insert session");
        store
            .update_session_state(&session.id, SessionState::Suspended)
            .expect("suspend session");
        store
            .update_session_presentation_state(&session.id, SessionPresentationState::Attached)
            .expect("attach presentation");

        let loaded = store
            .get_session(&session.id)
            .expect("get session")
            .expect("session exists");
        assert_eq!(loaded.state, SessionState::Suspended);
        assert_eq!(
            loaded.presentation_state,
            SessionPresentationState::Attached
        );
    }

    #[test]
    fn invalid_persisted_typed_id_is_not_silently_accepted() {
        let store = migrated_store();
        let flow = Flow::new("ID corruption test", "test");
        store.insert_flow(&flow).expect("insert flow");
        store
            .conn
            .execute(
                "UPDATE flows SET id = 'not-a-flow-id' WHERE id = ?1",
                params![flow.id.as_str()],
            )
            .expect("corrupt test row");

        assert!(store.list_flows().is_err());
    }

    #[test]
    fn invalid_persisted_goal_json_is_not_silently_accepted() {
        let store = migrated_store();
        let goal = Goal::new("Corruption test", "test");
        store.insert_goal(&goal).expect("insert goal");
        store
            .conn
            .execute(
                "UPDATE goals SET constraints = 'not-json' WHERE id = ?1",
                params![goal.id.as_str()],
            )
            .expect("corrupt test row");

        assert!(store.get_goal(&goal.id).is_err());
    }

    #[test]
    fn invalid_persisted_session_state_is_not_silently_accepted() {
        let store = migrated_store();
        let session = Session {
            owner: crate::types::Owner::User(1000),
            id: SessionId::new(),
            flow_id: None,
            kind: "local-pty".to_owned(),
            pid: None,
            cwd: None,
            state: SessionState::Running,
            presentation_state: SessionPresentationState::Detached,
            created_by: "test".to_owned(),
            created_at: Timestamp::now(),
        };
        store.insert_session(&session).expect("insert session");
        store
            .conn
            .execute(
                "UPDATE sessions SET state = 'CORRUPT' WHERE id = ?1",
                params![session.id.as_str()],
            )
            .expect("corrupt test row");

        assert!(store.get_session(&session.id).is_err());
    }

    #[test]
    fn missing_record_is_none_not_error() {
        let store = migrated_store();
        let missing = store.get_flow(&FlowId::new()).expect("query ok");
        assert!(missing.is_none());
    }

    #[test]
    fn state_transition_is_persisted() {
        let store = migrated_store();
        let flow = Flow::new("Art", "user");
        store.insert_flow(&flow).unwrap();
        store
            .set_flow_state(&flow.id, FlowState::Archived)
            .expect("update state");
        let loaded = store.get_flow(&flow.id).unwrap().unwrap();
        assert_eq!(loaded.state, FlowState::Archived);
    }

    #[test]
    fn all_core_persistent_objects_survive_store_reopen() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state.db");

        let mut flow = Flow::new("Persistent", "test");
        flow.owner = crate::types::Owner::User(1000);
        let mut goal = Goal::new("Persist the domain graph", "test");
        goal.owner = crate::types::Owner::User(1000);
        let session = Session {
            owner: crate::types::Owner::User(1000),
            id: SessionId::new(),
            flow_id: Some(flow.id.clone()),
            kind: "local-pty".to_owned(),
            pid: Some(4242),
            cwd: Some("/tmp".to_owned()),
            state: SessionState::Running,
            presentation_state: SessionPresentationState::Detached,
            created_by: "test".to_owned(),
            created_at: Timestamp::now(),
        };
        let execution = Execution {
            owner: crate::types::Owner::User(1000),
            id: ExecutionId::new(),
            agent_id: None,
            goal_id: Some(goal.id.clone()),
            reason: Some("persistence test".to_owned()),
            command: Some("true".to_owned()),
            exit_code: Some(0),
            duration_ms: Some(1),
            started_at: Timestamp::now(),
        };
        let generation = Generation {
            owner: crate::types::Owner::User(1000),
            id: GenerationId::new(),
            parent: None,
            source_commit: Some("deadbeef".to_owned()),
            system_manifest: Some("sha256:test".to_owned()),
            reason_goal: Some(goal.id.clone()),
            experiment: None,
            status: GenerationStatus::Candidate,
            created_at: Timestamp::now(),
        };
        let edge = ProvenanceEdge {
            from: ObjectRef::from(&execution.id),
            relation: ProvenanceRelation::CausedBy,
            to: ObjectRef::from(&goal.id),
            created_at: Timestamp::now(),
            note: Some("durable causal link".to_owned()),
        };

        {
            let mut writer = Store::open(&path).expect("open writer");
            writer.migrate().expect("migrate writer");
            writer.insert_flow(&flow).expect("insert flow");
            writer.insert_goal(&goal).expect("insert goal");
            writer.insert_session(&session).expect("insert session");
            writer
                .insert_execution_with_provenance(&execution, std::slice::from_ref(&edge))
                .expect("insert execution");
            writer
                .insert_generation(&generation)
                .expect("insert generation");
        }

        let mut reader = Store::open(&path).expect("reopen state");
        reader.migrate().expect("reopen migrations");

        assert_eq!(
            reader
                .get_flow(&flow.id)
                .expect("flow query")
                .expect("flow exists")
                .id,
            flow.id
        );
        assert_eq!(
            reader
                .get_goal(&goal.id)
                .expect("goal query")
                .expect("goal exists")
                .id,
            goal.id
        );
        assert_eq!(
            reader
                .get_session(&session.id)
                .expect("session query")
                .expect("session exists")
                .id,
            session.id
        );
        assert_eq!(
            reader
                .get_execution(&execution.id)
                .expect("execution query")
                .expect("execution exists")
                .id,
            execution.id
        );
        assert_eq!(
            reader
                .get_generation(&generation.id)
                .expect("generation query")
                .expect("generation exists")
                .id,
            generation.id
        );

        let outgoing = reader
            .provenance_from(&ObjectRef::from(&execution.id))
            .expect("outgoing provenance");
        let incoming = reader
            .provenance_to(&ObjectRef::from(&goal.id))
            .expect("incoming provenance");
        assert_eq!(outgoing, vec![edge.clone()]);
        assert_eq!(incoming, vec![edge]);
    }

    #[test]
    fn generation_activation_keeps_exactly_one_stable_generation() {
        let mut store = migrated_store();
        let first = Generation {
            owner: crate::types::Owner::System,
            id: GenerationId::new(),
            parent: None,
            source_commit: Some("first".to_owned()),
            system_manifest: None,
            reason_goal: None,
            experiment: None,
            status: GenerationStatus::Stable,
            created_at: Timestamp::now(),
        };
        let second = Generation {
            owner: crate::types::Owner::User(1000),
            id: GenerationId::new(),
            parent: Some(first.id.clone()),
            source_commit: Some("second".to_owned()),
            system_manifest: None,
            reason_goal: None,
            experiment: None,
            status: GenerationStatus::Candidate,
            created_at: Timestamp::now(),
        };
        store.insert_generation(&first).expect("insert first");
        store.insert_generation(&second).expect("insert second");

        // A private candidate must be promoted (audited) before activation.
        assert!(matches!(
            store.activate_generation(&second.id),
            Err(StorageError::OperationConflict(_))
        ));
        assert_eq!(
            store.get_generation(&first.id).unwrap().unwrap().status,
            GenerationStatus::Stable
        );
        store
            .promote_generation(Caller::new(0), &second.id, "release candidate")
            .expect("promote second");
        store
            .activate_generation(&second.id)
            .expect("activate second");

        let first = store
            .get_generation(&first.id)
            .expect("query first")
            .expect("first exists");
        let second = store
            .get_generation(&second.id)
            .expect("query second")
            .expect("second exists");

        assert_eq!(first.status, GenerationStatus::Superseded);
        assert_eq!(second.status, GenerationStatus::Stable);
        assert_eq!(
            store
                .list_generations()
                .expect("list generations")
                .iter()
                .filter(|generation| generation.status == GenerationStatus::Stable)
                .count(),
            1
        );
    }

    #[test]
    fn missing_generation_activation_does_not_supersede_current_stable() {
        let mut store = migrated_store();
        let stable = Generation {
            owner: crate::types::Owner::User(1000),
            id: GenerationId::new(),
            parent: None,
            source_commit: Some("stable".to_owned()),
            system_manifest: None,
            reason_goal: None,
            experiment: None,
            status: GenerationStatus::Stable,
            created_at: Timestamp::now(),
        };
        store.insert_generation(&stable).expect("insert stable");

        let missing = GenerationId::new();
        let error = store
            .activate_generation(&missing)
            .expect_err("missing generation must fail");
        assert!(matches!(error, StorageError::NotFound(_)));

        let stable = store
            .get_generation(&stable.id)
            .expect("query stable")
            .expect("stable exists");
        assert_eq!(stable.status, GenerationStatus::Stable);
    }

    #[test]
    fn invalid_persisted_generation_reference_is_not_silently_accepted() {
        let store = migrated_store();
        let generation = Generation {
            owner: crate::types::Owner::User(1000),
            id: GenerationId::new(),
            parent: None,
            source_commit: None,
            system_manifest: None,
            reason_goal: None,
            experiment: None,
            status: GenerationStatus::Candidate,
            created_at: Timestamp::now(),
        };
        store
            .insert_generation(&generation)
            .expect("insert generation");
        store
            .conn
            .execute(
                "UPDATE generations SET experiment = 'not-an-experiment-id' WHERE id = ?1",
                params![generation.id.as_str()],
            )
            .expect("corrupt generation");

        assert!(store.get_generation(&generation.id).is_err());
    }

    #[test]
    fn invalid_persisted_execution_reference_is_not_silently_accepted() {
        let store = migrated_store();
        let execution = Execution {
            owner: crate::types::Owner::User(1000),
            id: ExecutionId::new(),
            agent_id: None,
            goal_id: None,
            reason: None,
            command: Some("true".to_owned()),
            exit_code: Some(0),
            duration_ms: None,
            started_at: Timestamp::now(),
        };
        store
            .insert_execution(&execution)
            .expect("insert execution");
        store
            .conn
            .pragma_update(None, "foreign_keys", "OFF")
            .expect("disable foreign keys for corruption simulation");
        store
            .conn
            .execute(
                "UPDATE executions SET goal_id = 'not-a-goal-id' WHERE id = ?1",
                params![execution.id.as_str()],
            )
            .expect("corrupt execution");
        store
            .conn
            .pragma_update(None, "foreign_keys", "ON")
            .expect("restore foreign keys");

        assert!(store.get_execution(&execution.id).is_err());
    }
}
