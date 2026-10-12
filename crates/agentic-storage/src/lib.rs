#![forbid(unsafe_code)]

use std::path::Path;
use std::str::FromStr;

use agentic_types::{
    AgentId, Execution, ExecutionId, Flow, FlowId, FlowState, Generation, GenerationId,
    GenerationStatus, Goal, GoalId, GoalStatus, ObjectRef, ProvenanceEdge, Session, SessionId,
    SessionPresentationState, SessionState, Timestamp,
};
use rusqlite::types::Type;
use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};
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
        sql: include_str!("../migrations/0001_init.sql"),
    },
    Migration {
        version: 2,
        name: "session_presentation_and_actor_refs",
        sql: include_str!("../migrations/0002_session_presentation_and_actor_refs.sql"),
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMigration {
    pub version: i64,

    pub name: String,

    pub applied_at: Timestamp,
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

    pub fn insert_flow(&self, flow: &Flow) -> Result<()> {
        insert_flow_row(&self.conn, flow)
    }

    pub fn insert_flow_with_provenance(
        &mut self,
        flow: &Flow,
        edges: &[ProvenanceEdge],
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_flow_row(&tx, flow)?;
        for edge in edges {
            insert_provenance_row(&tx, edge)?;
        }
        tx.commit()?;
        Ok(())
    }

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

    pub fn list_flows(&self) -> Result<Vec<Flow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, purpose, parent_flow, state, created_by, created_at, last_active, revision
             FROM flows ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], map_flow)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Atomically move the foreground to one Flow in the shared state scope.
    /// Missing or archived targets leave the current foreground unchanged.
    pub fn activate_flow(&mut self, id: &FlowId) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let target: Option<String> = tx
            .query_row(
                "SELECT state FROM flows WHERE id = ?1",
                params![id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let target = target.ok_or_else(|| StorageError::NotFound(id.to_string()))?;
        if target == "ARCHIVED" {
            return Err(StorageError::NotFound(format!("archived flow: {id}")));
        }
        tx.execute(
            "UPDATE flows SET state = 'ACTIVE', revision = revision + 1 WHERE state = 'FOREGROUND' AND id != ?1",
            params![id.as_str()],
        )?;
        tx.execute(
            "UPDATE flows SET state = 'FOREGROUND', last_active = ?2, revision = revision + 1 WHERE id = ?1",
            params![id.as_str(), Timestamp::now().as_millis()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_flow_state(&self, id: &FlowId, state: FlowState) -> Result<()> {
        if state == FlowState::Foreground {
            return Err(StorageError::NotFound(
                "use activate_flow for foreground transitions".to_owned(),
            ));
        }

        let changed = self.conn.execute(
            "UPDATE flows SET state = ?2 WHERE id = ?1",
            params![id.as_str(), state.to_string()],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound(id.to_string()));
        }
        Ok(())
    }

    pub fn insert_goal(&self, goal: &Goal) -> Result<()> {
        insert_goal_row(&self.conn, goal)
    }

    pub fn insert_goal_with_provenance(
        &mut self,
        goal: &Goal,
        edges: &[ProvenanceEdge],
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_goal_row(&tx, goal)?;
        for edge in edges {
            insert_provenance_row(&tx, edge)?;
        }
        tx.commit()?;
        Ok(())
    }

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

    pub fn list_goals(&self) -> Result<Vec<Goal>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_goal, objective, rationale, constraints, success_metrics, priority, status, created_by, created_at
             FROM goals ORDER BY priority DESC, created_at",
        )?;
        let rows = stmt.query_map([], map_goal)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

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

    pub fn insert_session(&self, session: &Session) -> Result<()> {
        insert_session_row(&self.conn, session)
    }

    pub fn insert_session_with_provenance(
        &mut self,
        session: &Session,
        edges: &[ProvenanceEdge],
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_session_row(&tx, session)?;
        for edge in edges {
            insert_provenance_row(&tx, edge)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_session(&self, id: &SessionId) -> Result<Option<Session>> {
        self.conn
            .query_row(
                "SELECT id, flow_id, kind, pid, cwd, state, presentation_state, created_by, created_at
                 FROM sessions WHERE id = ?1",
                params![id.as_str()],
                map_session,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, flow_id, kind, pid, cwd, state, presentation_state, created_by, created_at
             FROM sessions ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], map_session)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

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
        insert_execution_row(&self.conn, exec)
    }

    pub fn insert_execution_with_provenance(
        &mut self,
        exec: &Execution,
        edges: &[ProvenanceEdge],
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_execution_row(&tx, exec)?;
        for edge in edges {
            insert_provenance_row(&tx, edge)?;
        }
        tx.commit()?;
        Ok(())
    }

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

    pub fn list_executions(&self) -> Result<Vec<Execution>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, agent_id, goal_id, reason, command, exit_code, duration_ms, started_at
             FROM executions ORDER BY started_at",
        )?;
        let rows = stmt.query_map([], map_execution)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn insert_generation(&self, generation: &Generation) -> Result<()> {
        insert_generation_row(&self.conn, generation)
    }

    pub fn insert_generation_with_provenance(
        &mut self,
        generation: &Generation,
        edges: &[ProvenanceEdge],
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_generation_row(&tx, generation)?;
        for edge in edges {
            insert_provenance_row(&tx, edge)?;
        }
        tx.commit()?;
        Ok(())
    }

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

    pub fn list_generations(&self) -> Result<Vec<Generation>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent, source_commit, system_manifest, reason_goal, experiment, status, created_at
             FROM generations ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], map_generation)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

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

    /// Promote one generation to STABLE and supersede any previous STABLE
    /// generation in the same SQLite transaction.
    pub fn activate_generation(&mut self, id: &GenerationId) -> Result<()> {
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

fn insert_flow_row(conn: &Connection, flow: &Flow) -> Result<()> {
    conn.execute(
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

fn insert_goal_row(conn: &Connection, goal: &Goal) -> Result<()> {
    conn.execute(
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

fn insert_session_row(conn: &Connection, session: &Session) -> Result<()> {
    conn.execute(
        "INSERT INTO sessions (id, flow_id, kind, pid, cwd, state, presentation_state, created_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
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
        ],
    )?;
    Ok(())
}

fn insert_execution_row(conn: &Connection, exec: &Execution) -> Result<()> {
    conn.execute(
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

fn insert_generation_row(conn: &Connection, generation: &Generation) -> Result<()> {
    conn.execute(
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

fn parse_text<T>(raw: String, column: usize) -> rusqlite::Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    raw.parse().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(column, Type::Text, Box::new(error))
    })
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
    use super::*;
    use agentic_types::ProvenanceRelation;

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

        let applied = store.migrate().expect("apply migration two");
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].version, 2);

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

        let flow = Flow::new("Persistent", "test");
        let goal = Goal::new("Persist the domain graph", "test");
        let session = Session {
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
