//! Durable SQLite-backed state for Flowerly OS.
//!
//! Every schema change is expressed as an ordered [`Migration`]. The
//! migration runner is idempotent: applying it twice is safe and only records
//! the migrations that were not already applied.

#![forbid(unsafe_code)]

use std::path::Path;

use agentic_types::{
    AgentId, Execution, ExecutionId, Flow, FlowId, FlowState, Generation, GenerationId,
    GenerationStatus, Goal, GoalId, GoalStatus, ObjectRef, ProvenanceEdge, ProvenanceRelation,
    Session, SessionId, SessionState, Timestamp,
};
use rusqlite::{params, Connection, OptionalExtension, Row};
use thiserror::Error;

/// Errors produced by the storage layer.
#[derive(Debug, Error)]
pub enum StorageError {
    /// Underlying SQLite failure.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// (De)serialization failure for embedded JSON columns.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    /// A record was requested but does not exist.
    #[error("record not found: {0}")]
    NotFound(String),
}

/// Result alias for the storage layer.
pub type Result<T> = std::result::Result<T, StorageError>;

/// A single, ordered schema migration.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    /// Monotonic version number.
    pub version: i64,
    /// Human-readable name.
    pub name: &'static str,
    /// SQL to execute.
    pub sql: &'static str,
}

/// The complete, ordered migration set.
pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "init",
    sql: include_str!("../migrations/0001_init.sql"),
}];

/// A migration that has been applied to a database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMigration {
    /// Version number.
    pub version: i64,
    /// Name recorded when applied.
    pub name: String,
    /// When it was applied.
    pub applied_at: Timestamp,
}

/// A handle to a durable Flowerly OS state store.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if necessary) a database at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::configure(&conn)?;
        Ok(Self { conn })
    }

    /// Open an ephemeral, in-memory database (used by tests).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::configure(&conn)?;
        Ok(Self { conn })
    }

    fn configure(conn: &Connection) -> Result<()> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // WAL improves concurrent read behaviour; harmless for in-memory DBs.
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(())
    }

    /// Apply all pending migrations, returning the ones that were newly applied.
    pub fn migrate(&mut self) -> Result<Vec<AppliedMigration>> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                 version    INTEGER PRIMARY KEY,
                 name       TEXT NOT NULL,
                 applied_at INTEGER NOT NULL
             );",
        )?;

        let already: Vec<i64> = {
            let mut stmt = self.conn.prepare("SELECT version FROM schema_migrations")?;
            let rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        let mut applied = Vec::new();
        for migration in MIGRATIONS {
            if already.contains(&migration.version) {
                continue;
            }
            let tx_record = Timestamp::now();
            let tx = self.conn.transaction()?;
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

    /// List migrations already applied to this database.
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

    /// Verify that the database answers a trivial query (integrity smoke test).
    pub fn health_check(&self) -> Result<()> {
        let value: i64 = self.conn.query_row("SELECT 1", [], |row| row.get(0))?;
        if value != 1 {
            return Err(StorageError::NotFound("database health check".into()));
        }
        Ok(())
    }

    // ---------------------------------------------------------------- flows

    /// Insert a Flow.
    pub fn insert_flow(&self, flow: &Flow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO flows (id, name, purpose, parent_flow, state, created_by, created_at, last_active, revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
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
            ],
        )?;
        Ok(())
    }

    /// Fetch a Flow by id.
    pub fn get_flow(&self, id: &FlowId) -> Result<Option<Flow>> {
        self.conn
            .query_row(
                "SELECT id, name, purpose, parent_flow, state, created_by, created_at, last_active, revision
                 FROM flows WHERE id = ?1",
                params![id.as_str()],
                map_flow,
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// List all Flows ordered by creation time.
    pub fn list_flows(&self) -> Result<Vec<Flow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, purpose, parent_flow, state, created_by, created_at, last_active, revision
             FROM flows ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], map_flow)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Update a Flow's lifecycle state.
    pub fn set_flow_state(&self, id: &FlowId, state: FlowState) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE flows SET state = ?2 WHERE id = ?1",
            params![id.as_str(), state.to_string()],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound(id.to_string()));
        }
        Ok(())
    }

    // ---------------------------------------------------------------- goals

    /// Insert a Goal.
    pub fn insert_goal(&self, goal: &Goal) -> Result<()> {
        self.conn.execute(
            "INSERT INTO goals (id, parent_goal, objective, rationale, constraints, success_metrics, priority, status, created_by, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
            ],
        )?;
        Ok(())
    }

    /// Fetch a Goal by id.
    pub fn get_goal(&self, id: &GoalId) -> Result<Option<Goal>> {
        self.conn
            .query_row(
                "SELECT id, parent_goal, objective, rationale, constraints, success_metrics, priority, status, created_by, created_at
                 FROM goals WHERE id = ?1",
                params![id.as_str()],
                map_goal,
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// List all Goals.
    pub fn list_goals(&self) -> Result<Vec<Goal>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_goal, objective, rationale, constraints, success_metrics, priority, status, created_by, created_at
             FROM goals ORDER BY priority DESC, created_at",
        )?;
        let rows = stmt.query_map([], map_goal)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Update a Goal's status.
    pub fn set_goal_status(&self, id: &GoalId, status: GoalStatus) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE goals SET status = ?2 WHERE id = ?1",
            params![id.as_str(), status.to_string()],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound(id.to_string()));
        }
        Ok(())
    }

    // ------------------------------------------------------------- sessions

    /// Insert a Session.
    pub fn insert_session(&self, session: &Session) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sessions (id, flow_id, kind, pid, cwd, state, created_by, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                session.id.as_str(),
                session.flow_id.as_ref().map(FlowId::as_str),
                session.kind,
                session.pid,
                session.cwd,
                session.state.to_string(),
                session.created_by,
                session.created_at.as_millis(),
            ],
        )?;
        Ok(())
    }

    /// Fetch a Session by id.
    pub fn get_session(&self, id: &SessionId) -> Result<Option<Session>> {
        self.conn
            .query_row(
                "SELECT id, flow_id, kind, pid, cwd, state, created_by, created_at
                 FROM sessions WHERE id = ?1",
                params![id.as_str()],
                map_session,
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// List all Sessions.
    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, flow_id, kind, pid, cwd, state, created_by, created_at
             FROM sessions ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], map_session)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Update a Session's lifecycle state.
    pub fn update_session_state(&self, id: &SessionId, state: SessionState) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE sessions SET state = ?2 WHERE id = ?1",
            params![id.as_str(), state.to_string()],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound(id.to_string()));
        }
        Ok(())
    }

    // ----------------------------------------------------------- executions

    /// Insert an Execution record.
    pub fn insert_execution(&self, exec: &Execution) -> Result<()> {
        self.conn.execute(
            "INSERT INTO executions (id, agent_id, goal_id, reason, command, exit_code, duration_ms, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                exec.id.as_str(),
                exec.agent_id.as_ref().map(AgentId::as_str),
                exec.goal_id.as_ref().map(GoalId::as_str),
                exec.reason,
                exec.command,
                exec.exit_code,
                exec.duration_ms,
                exec.started_at.as_millis(),
            ],
        )?;
        Ok(())
    }

    /// Fetch an Execution by id.
    pub fn get_execution(&self, id: &ExecutionId) -> Result<Option<Execution>> {
        self.conn
            .query_row(
                "SELECT id, agent_id, goal_id, reason, command, exit_code, duration_ms, started_at
                 FROM executions WHERE id = ?1",
                params![id.as_str()],
                map_execution,
            )
            .optional()
            .map_err(StorageError::from)
    }

    // ---------------------------------------------------------- generations

    /// Insert a Generation.
    pub fn insert_generation(&self, generation: &Generation) -> Result<()> {
        self.conn.execute(
            "INSERT INTO generations (id, parent, source_commit, system_manifest, reason_goal, experiment, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                generation.id.as_str(),
                generation.parent.as_ref().map(GenerationId::as_str),
                generation.source_commit,
                generation.system_manifest,
                generation.reason_goal.as_ref().map(GoalId::as_str),
                generation.experiment.as_ref().map(|e| e.to_string()),
                generation.status.to_string(),
                generation.created_at.as_millis(),
            ],
        )?;
        Ok(())
    }

    /// Fetch a Generation by id.
    pub fn get_generation(&self, id: &GenerationId) -> Result<Option<Generation>> {
        self.conn
            .query_row(
                "SELECT id, parent, source_commit, system_manifest, reason_goal, experiment, status, created_at
                 FROM generations WHERE id = ?1",
                params![id.as_str()],
                map_generation,
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// List all Generations, newest last.
    pub fn list_generations(&self) -> Result<Vec<Generation>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent, source_commit, system_manifest, reason_goal, experiment, status, created_at
             FROM generations ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], map_generation)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Update a Generation's lifecycle status.
    pub fn set_generation_status(&self, id: &GenerationId, status: GenerationStatus) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE generations SET status = ?2 WHERE id = ?1",
            params![id.as_str(), status.to_string()],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound(id.to_string()));
        }
        Ok(())
    }

    // -------------------------------------------------------- provenance

    /// Insert a provenance edge. Duplicate edges are ignored.
    pub fn insert_provenance(&self, edge: &ProvenanceEdge) -> Result<()> {
        self.conn.execute(
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

    /// List provenance edges originating from an object.
    pub fn provenance_from(&self, from: &ObjectRef) -> Result<Vec<ProvenanceEdge>> {
        let mut stmt = self.conn.prepare(
            "SELECT from_ref, relation, to_ref, created_at, note
             FROM provenance_edges WHERE from_ref = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![from.as_str()], map_provenance)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

fn map_flow(row: &Row<'_>) -> rusqlite::Result<Flow> {
    let parent: Option<String> = row.get(3)?;
    let last_active: Option<i64> = row.get(7)?;
    let state: String = row.get(4)?;
    Ok(Flow {
        id: FlowId::from_raw(row.get::<_, String>(0)?),
        name: row.get(1)?,
        purpose: row.get(2)?,
        parent_flow: parent.map(FlowId::from_raw),
        state: state.parse().unwrap_or(FlowState::Active),
        created_by: row.get(5)?,
        created_at: Timestamp::from_millis(row.get(6)?),
        last_active: last_active.map(Timestamp::from_millis),
        revision: row.get(8)?,
    })
}

fn map_goal(row: &Row<'_>) -> rusqlite::Result<Goal> {
    let parent: Option<String> = row.get(1)?;
    let constraints: String = row.get(4)?;
    let metrics: String = row.get(5)?;
    let status: String = row.get(7)?;
    Ok(Goal {
        id: GoalId::from_raw(row.get::<_, String>(0)?),
        parent_goal: parent.map(GoalId::from_raw),
        objective: row.get(2)?,
        rationale: row.get(3)?,
        constraints: serde_json::from_str(&constraints).unwrap_or_default(),
        success_metrics: serde_json::from_str(&metrics).unwrap_or_default(),
        priority: row.get(6)?,
        status: status.parse().unwrap_or(GoalStatus::Proposed),
        created_by: row.get(8)?,
        created_at: Timestamp::from_millis(row.get(9)?),
    })
}

fn map_session(row: &Row<'_>) -> rusqlite::Result<Session> {
    let flow: Option<String> = row.get(1)?;
    let state: String = row.get(5)?;
    Ok(Session {
        id: SessionId::from_raw(row.get::<_, String>(0)?),
        flow_id: flow.map(FlowId::from_raw),
        kind: row.get(2)?,
        pid: row.get(3)?,
        cwd: row.get(4)?,
        state: match state.as_str() {
            "RUNNING" => SessionState::Running,
            "EXITED" => SessionState::Exited,
            _ => SessionState::Detached,
        },
        created_by: row.get(6)?,
        created_at: Timestamp::from_millis(row.get(7)?),
    })
}

fn map_execution(row: &Row<'_>) -> rusqlite::Result<Execution> {
    let agent: Option<String> = row.get(1)?;
    let goal: Option<String> = row.get(2)?;
    Ok(Execution {
        id: ExecutionId::from_raw(row.get::<_, String>(0)?),
        agent_id: agent.map(AgentId::from_raw),
        goal_id: goal.map(GoalId::from_raw),
        reason: row.get(3)?,
        command: row.get(4)?,
        exit_code: row.get(5)?,
        duration_ms: row.get(6)?,
        started_at: Timestamp::from_millis(row.get(7)?),
    })
}

fn map_generation(row: &Row<'_>) -> rusqlite::Result<Generation> {
    let parent: Option<String> = row.get(1)?;
    let goal: Option<String> = row.get(4)?;
    let experiment: Option<String> = row.get(5)?;
    let status: String = row.get(6)?;
    Ok(Generation {
        id: GenerationId::from_raw(row.get::<_, String>(0)?),
        parent: parent.map(GenerationId::from_raw),
        source_commit: row.get(2)?,
        system_manifest: row.get(3)?,
        reason_goal: goal.map(GoalId::from_raw),
        experiment: experiment.map(agentic_types::ExperimentId::from_raw),
        status: match status.as_str() {
            "CANDIDATE" => GenerationStatus::Candidate,
            "TESTED" => GenerationStatus::Tested,
            "STABLE" => GenerationStatus::Stable,
            "REJECTED" => GenerationStatus::Rejected,
            _ => GenerationStatus::Superseded,
        },
        created_at: Timestamp::from_millis(row.get(7)?),
    })
}

fn map_provenance(row: &Row<'_>) -> rusqlite::Result<ProvenanceEdge> {
    let relation: String = row.get(1)?;
    let relation = match relation.as_str() {
        "created_by" => ProvenanceRelation::CreatedBy,
        "caused_by" => ProvenanceRelation::CausedBy,
        "belongs_to" => ProvenanceRelation::BelongsTo,
        "produced" => ProvenanceRelation::Produced,
        "tested_by" => ProvenanceRelation::TestedBy,
        _ => ProvenanceRelation::PromotedTo,
    };
    Ok(ProvenanceEdge {
        from: ObjectRef::new(row.get::<_, String>(0)?),
        relation,
        to: ObjectRef::new(row.get::<_, String>(2)?),
        created_at: Timestamp::from_millis(row.get(3)?),
        note: row.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migrated_store() -> Store {
        let mut store = Store::open_in_memory().expect("open store");
        store.migrate().expect("migrate");
        store
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
            from: ObjectRef::new(goal.id.to_string()),
            relation: ProvenanceRelation::BelongsTo,
            to: ObjectRef::new(flow.id.to_string()),
            created_at: Timestamp::now(),
            note: None,
        };
        store.insert_provenance(&edge).expect("insert edge");
        // Idempotent.
        store.insert_provenance(&edge).expect("insert edge again");

        let edges = store
            .provenance_from(&ObjectRef::new(goal.id.to_string()))
            .expect("query edges");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].relation, ProvenanceRelation::BelongsTo);
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
}
