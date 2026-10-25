//! Owner-scoped reads and mutations (#225).
//!
//! Every method takes the kernel-attested [`Caller`] explicitly; the shared
//! connection has no ambient "current user". Visibility is decided in the SQL
//! of the same statement or IMMEDIATE transaction as any mutation:
//!
//! - Flows: the caller's own Flows and the canonical General system Flow.
//! - Goals, Sessions, Executions: the caller's own rows only.
//!
//! Anything else (another uid's, unresolved or non-General system rows) is
//! indistinguishable from a missing row: lookups return `None` and mutations
//! `NotFound` with the same message, so there is no existence oracle.
//!
//! The foreground is per owner (`flow_foreground`, migration 0010). No row
//! means General; activating General deletes only the caller's row.
//!
//! Provenance (PR 4) is filtered by the visibility of **both** endpoints; see
//! [`ref_visibility`].

use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};

use super::Store;
use super::{
    map_execution, map_flow, map_goal, map_provenance, map_session, Result, StatusCounts,
    StorageError,
};
use crate::types::{
    Caller, Execution, ExecutionId, Flow, FlowId, Goal, GoalId, GoalStatus, ObjectRef, Owner,
    ProvenanceEdge, Session, SessionId, SessionPresentationState, SessionState, Timestamp,
};

/// SQL predicate for the canonical General Flow (unique by 0004).
const GENERAL: &str = "f.name = 'General' AND f.owner_class = 'system'";

/// A Flow as one caller sees it: its lifecycle `state` and, separately,
/// whether it is that caller's effective foreground.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowView {
    pub flow: Flow,
    pub foreground: bool,
}

impl FlowView {
    #[must_use]
    pub fn is_general(&self) -> bool {
        self.flow.name == "General" && self.flow.owner == Owner::System
    }
}

/// Visible Flows for `?1` with the caller's foreground computed in the same
/// statement. Callers append further `AND` conditions.
fn flow_view_sql(extra: &str) -> String {
    format!(
        "SELECT f.id, f.name, f.purpose, f.parent_flow, f.state, f.created_by, f.created_at,
                f.last_active, f.revision, f.owner_class, f.owner_uid,
                (fg.flow_id IS NOT NULL OR ({GENERAL} AND NOT EXISTS (
                    SELECT 1 FROM flow_foreground AS mine WHERE mine.owner_uid = ?1))) AS foreground
         FROM flows AS f
         LEFT JOIN flow_foreground AS fg ON fg.flow_id = f.id AND fg.owner_uid = ?1
         WHERE ((f.owner_class = 'user' AND f.owner_uid = ?1) OR ({GENERAL})) {extra}"
    )
}

fn map_flow_view(row: &Row<'_>) -> rusqlite::Result<FlowView> {
    Ok(FlowView {
        flow: map_flow(row)?,
        foreground: row.get(11)?,
    })
}

fn flow_view(conn: &Connection, caller: Caller, id: &FlowId) -> Result<Option<FlowView>> {
    conn.query_row(
        &flow_view_sql("AND f.id = ?2"),
        params![caller.uid(), id.as_str()],
        map_flow_view,
    )
    .optional()
    .map_err(StorageError::from)
}

fn not_found(kind: &str, id: &str) -> StorageError {
    StorageError::NotFound(format!("{kind} {id}"))
}

const GOAL_COLUMNS: &str = "id, parent_goal, objective, rationale, constraints, success_metrics, \
     priority, status, created_by, created_at, owner_class, owner_uid";
const SESSION_COLUMNS: &str = "id, flow_id, kind, pid, cwd, state, presentation_state, \
     created_by, created_at, owner_class, owner_uid";
const EXECUTION_COLUMNS: &str = "id, agent_id, goal_id, reason, command, exit_code, duration_ms, \
     started_at, owner_class, owner_uid";
const OWNED: &str = "owner_class = 'user' AND owner_uid = ?1";
const OWN_OR_SYSTEM: &str = "((owner_class = 'user' AND owner_uid = ?1) OR owner_class = 'system')";

/// SQL expression for how a provenance endpoint stored in `column` appears to
/// the caller bound to `?1`:
///
/// - `1` (visible): the caller's own Flow, Goal, Session, Execution,
///   Generation, profile or workload; General; a `system` Generation,
///   profile or workload; a shared catalog model or provider; the caller's
///   own `actor:uid:<n>` label.
/// - `0` (hidden): any other row of those kinds, a missing one, or
///   `actor:uid:<n>` of another uid.
/// - `NULL` (neutral): kinds with no owner yet (agents, experiments,
///   findings, …) and other actor labels.
///
/// An edge is visible only when no endpoint is hidden and at least one is
/// visible, so neutral-to-neutral edges are never shown to ordinary users.
fn ref_visibility(column: &str) -> String {
    let own_or_system = |table: &str| {
        format!(
            "EXISTS(SELECT 1 FROM {table} AS o WHERE o.id = {column} AND \
             ((o.owner_class = 'user' AND o.owner_uid = ?1) OR o.owner_class = 'system'))"
        )
    };
    let own = |table: &str| {
        format!(
            "EXISTS(SELECT 1 FROM {table} AS o WHERE o.id = {column} AND \
             o.owner_class = 'user' AND o.owner_uid = ?1)"
        )
    };
    format!(
        "(CASE
            WHEN {column} GLOB 'flow-*' THEN EXISTS(SELECT 1 FROM flows AS f
                WHERE f.id = {column} AND ((f.owner_class = 'user' AND f.owner_uid = ?1)
                                           OR ({GENERAL})))
            WHEN {column} GLOB 'goal-*' THEN {goals}
            WHEN {column} GLOB 'session-*' THEN {sessions}
            WHEN {column} GLOB 'exec-*' THEN {executions}
            WHEN {column} GLOB 'gen-*' THEN {generations}
            WHEN {column} GLOB 'profile-*' THEN {profiles}
            WHEN {column} GLOB 'workload-*' THEN {workloads}
            WHEN {column} GLOB 'model-*' OR {column} GLOB 'provider-*' THEN 1
            WHEN {column} GLOB 'actor:uid:*' THEN {column} = 'actor:uid:' || ?1
            ELSE NULL
         END)",
        goals = own("goals"),
        sessions = own("sessions"),
        executions = own("executions"),
        generations = own_or_system("generations"),
        profiles = own_or_system("ai_profiles"),
        workloads = own_or_system("inference_workloads"),
    )
}

/// Predicate on `provenance_edges AS e` for edges visible to `?1`.
fn visible_edge() -> String {
    let from = ref_visibility("e.from_ref");
    let to = ref_visibility("e.to_ref");
    format!("({from} IS NOT 0 AND {to} IS NOT 0 AND ({from} IS 1 OR {to} IS 1))")
}

/// A new row may reference only a Goal with the same owner. Checked inside
/// the inserting transaction; the 0010 triggers back it up.
pub(super) fn require_same_owner_goal(conn: &Connection, owner: Owner, id: &GoalId) -> Result<()> {
    let found: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM goals
                       WHERE id = ?1 AND owner_class = ?2 AND owner_uid IS ?3)",
        params![id.as_str(), owner.class(), owner.uid()],
        |row| row.get(0),
    )?;
    if found {
        Ok(())
    } else {
        Err(not_found("goal", id.as_str()))
    }
}

/// A new Session may reference only a Flow with the same owner or General.
pub(super) fn require_usable_flow(conn: &Connection, owner: Owner, id: &FlowId) -> Result<()> {
    let found: bool = conn.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM flows AS f WHERE f.id = ?1 AND (
                 (f.owner_class = ?2 AND f.owner_uid IS ?3) OR ({GENERAL})))"
        ),
        params![id.as_str(), owner.class(), owner.uid()],
        |row| row.get(0),
    )?;
    if found {
        Ok(())
    } else {
        Err(not_found("flow", id.as_str()))
    }
}

impl Store {
    /// Flows visible to `caller`, oldest first.
    pub fn flows_for(&self, caller: Caller) -> Result<Vec<FlowView>> {
        let mut stmt = self
            .conn
            .prepare(&flow_view_sql("ORDER BY f.created_at, f.id"))?;
        let rows = stmt.query_map(params![caller.uid()], map_flow_view)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn flow_for(&self, caller: Caller, id: &FlowId) -> Result<Option<FlowView>> {
        flow_view(&self.conn, caller, id)
    }

    /// The caller's effective foreground: its own row, else General.
    pub fn foreground_flow_for(&self, caller: Caller) -> Result<FlowView> {
        self.conn
            .query_row(
                &flow_view_sql("AND foreground"),
                params![caller.uid()],
                map_flow_view,
            )
            .optional()?
            .ok_or_else(|| not_found("flow", "General"))
    }

    /// Make `id` the caller's foreground. General removes only the caller's
    /// row. Another uid's, unresolved or missing Flow is `NotFound`; the
    /// caller's archived Flow is a conflict. Other owners are never touched.
    pub fn activate_flow_for(&mut self, caller: Caller, id: &FlowId) -> Result<FlowView> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let target = flow_view(&tx, caller, id)?.ok_or_else(|| not_found("flow", id.as_str()))?;
        if target.is_general() {
            tx.execute(
                "DELETE FROM flow_foreground WHERE owner_uid = ?1",
                params![caller.uid()],
            )?;
        } else if target.flow.state == crate::types::FlowState::Archived {
            return Err(StorageError::OperationConflict(format!(
                "archived flow: {id}"
            )));
        } else {
            let now = Timestamp::now().as_millis();
            tx.execute(
                "INSERT INTO flow_foreground (owner_uid, flow_id, activated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(owner_uid) DO UPDATE
                 SET flow_id = excluded.flow_id, activated_at = excluded.activated_at",
                params![caller.uid(), id.as_str(), now],
            )?;
            tx.execute(
                "UPDATE flows SET last_active = ?3, revision = revision + 1
                 WHERE id = ?2 AND owner_class = 'user' AND owner_uid = ?1",
                params![caller.uid(), id.as_str(), now],
            )?;
        }
        let view = flow_view(&tx, caller, id)?.ok_or_else(|| not_found("flow", id.as_str()))?;
        tx.commit()?;
        Ok(view)
    }

    /// Archive one of the caller's Flows. If it was the caller's foreground,
    /// the caller falls back to General (0010 trigger, same statement).
    pub fn archive_flow_for(&mut self, caller: Caller, id: &FlowId) -> Result<FlowView> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let target = flow_view(&tx, caller, id)?.ok_or_else(|| not_found("flow", id.as_str()))?;
        if target.is_general() {
            return Err(StorageError::PermissionDenied(
                "General cannot be archived".to_owned(),
            ));
        }
        tx.execute(
            "UPDATE flows SET state = 'ARCHIVED', revision = revision + 1
             WHERE id = ?2 AND owner_class = 'user' AND owner_uid = ?1",
            params![caller.uid(), id.as_str()],
        )?;
        let view = flow_view(&tx, caller, id)?.ok_or_else(|| not_found("flow", id.as_str()))?;
        tx.commit()?;
        Ok(view)
    }

    pub fn goals_for(&self, caller: Caller) -> Result<Vec<Goal>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {GOAL_COLUMNS} FROM goals WHERE {OWNED} ORDER BY priority DESC, created_at"
        ))?;
        let rows = stmt.query_map(params![caller.uid()], map_goal)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn goal_for(&self, caller: Caller, id: &GoalId) -> Result<Option<Goal>> {
        self.conn
            .query_row(
                &format!("SELECT {GOAL_COLUMNS} FROM goals WHERE {OWNED} AND id = ?2"),
                params![caller.uid(), id.as_str()],
                map_goal,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn set_goal_status_for(
        &self,
        caller: Caller,
        id: &GoalId,
        status: GoalStatus,
    ) -> Result<()> {
        let changed = self.conn.execute(
            &format!("UPDATE goals SET status = ?3 WHERE {OWNED} AND id = ?2"),
            params![caller.uid(), id.as_str(), status.to_string()],
        )?;
        if changed == 0 {
            return Err(not_found("goal", id.as_str()));
        }
        Ok(())
    }

    pub fn sessions_for(&self, caller: Caller) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SESSION_COLUMNS} FROM sessions WHERE {OWNED} ORDER BY created_at"
        ))?;
        let rows = stmt.query_map(params![caller.uid()], map_session)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn session_for(&self, caller: Caller, id: &SessionId) -> Result<Option<Session>> {
        self.conn
            .query_row(
                &format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE {OWNED} AND id = ?2"),
                params![caller.uid(), id.as_str()],
                map_session,
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// Mark one of the caller's Sessions exited and detached in one
    /// transaction; returns the row as it was before.
    pub fn terminate_session_for(&mut self, caller: Caller, id: &SessionId) -> Result<Session> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let before = tx
            .query_row(
                &format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE {OWNED} AND id = ?2"),
                params![caller.uid(), id.as_str()],
                map_session,
            )
            .optional()?
            .ok_or_else(|| not_found("session", id.as_str()))?;
        tx.execute(
            &format!(
                "UPDATE sessions SET state = ?3, presentation_state = ?4 WHERE {OWNED} AND id = ?2"
            ),
            params![
                caller.uid(),
                id.as_str(),
                SessionState::Exited.to_string(),
                SessionPresentationState::Detached.to_string()
            ],
        )?;
        tx.commit()?;
        Ok(before)
    }

    pub fn executions_for(&self, caller: Caller) -> Result<Vec<Execution>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {EXECUTION_COLUMNS} FROM executions WHERE {OWNED} ORDER BY started_at"
        ))?;
        let rows = stmt.query_map(params![caller.uid()], map_execution)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn execution_for(&self, caller: Caller, id: &ExecutionId) -> Result<Option<Execution>> {
        self.conn
            .query_row(
                &format!("SELECT {EXECUTION_COLUMNS} FROM executions WHERE {OWNED} AND id = ?2"),
                params![caller.uid(), id.as_str()],
                map_execution,
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// Provenance edges leaving `from` that the caller may see. An invisible
    /// or missing `from` yields no edges, so the answer is the same.
    pub fn provenance_from_for(
        &self,
        caller: Caller,
        from: &ObjectRef,
    ) -> Result<Vec<ProvenanceEdge>> {
        self.visible_edges(caller, "e.from_ref", from)
    }

    /// Provenance edges arriving at `to` that the caller may see.
    pub fn provenance_to_for(&self, caller: Caller, to: &ObjectRef) -> Result<Vec<ProvenanceEdge>> {
        self.visible_edges(caller, "e.to_ref", to)
    }

    fn visible_edges(
        &self,
        caller: Caller,
        column: &str,
        endpoint: &ObjectRef,
    ) -> Result<Vec<ProvenanceEdge>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT e.from_ref, e.relation, e.to_ref, e.created_at, e.note
             FROM provenance_edges AS e
             WHERE {column} = ?2 AND {visible}
             ORDER BY e.id",
            visible = visible_edge()
        ))?;
        let rows = stmt.query_map(params![caller.uid(), endpoint.as_str()], map_provenance)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Status counts as `caller` may see them, from one snapshot. Every count
    /// is caller-scoped except the shared model and provider catalogs.
    /// Generations, profiles and workloads count the caller's own and
    /// `system` rows; provenance counts edges visible by both endpoints.
    pub fn status_counts_for(&self, caller: Caller) -> Result<StatusCounts> {
        let count = |row: &Row<'_>, index: usize| -> rusqlite::Result<u64> {
            let value: i64 = row.get(index)?;
            u64::try_from(value).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    index,
                    rusqlite::types::Type::Integer,
                    Box::new(error),
                )
            })
        };
        let counts = self.conn.query_row(
            &format!(
                "SELECT
                     (SELECT COUNT(*) FROM flows AS f
                      WHERE (f.owner_class = 'user' AND f.owner_uid = ?1) OR ({GENERAL})),
                     (SELECT COUNT(*) FROM goals WHERE {OWNED}),
                     (SELECT COUNT(*) FROM sessions WHERE {OWNED}),
                     (SELECT COUNT(*) FROM executions WHERE {OWNED}),
                     (SELECT COUNT(*) FROM generations WHERE {OWN_OR_SYSTEM}),
                     (SELECT COUNT(*) FROM provenance_edges AS e WHERE {visible}),
                     (SELECT COUNT(*) FROM models WHERE retired_at IS NULL),
                     (SELECT COUNT(*) FROM ai_profiles WHERE {OWN_OR_SYSTEM}),
                     (SELECT COUNT(*) FROM ai_providers WHERE retired_at IS NULL),
                     (SELECT COUNT(*) FROM inference_workloads
                      WHERE state IN ('QUEUED', 'ADMITTED') AND {OWN_OR_SYSTEM})",
                visible = visible_edge()
            ),
            params![caller.uid()],
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
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Barrier};

    use rusqlite::params;

    use super::super::{Store, MIGRATIONS};
    use super::*;
    use crate::ipc::ErrorCode;
    use crate::types::{FlowState, GoalStatus};

    const ALICE: Caller = Caller::new(1001);
    const BOB: Caller = Caller::new(1002);

    fn open(path: &Path) -> Store {
        let mut store = Store::open(path).expect("open");
        store.migrate().expect("migrate");
        store.ensure_general_flow().expect("General");
        store
    }

    fn file_store() -> (tempfile::TempDir, std::path::PathBuf, Store) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state.db");
        let store = open(&path);
        (dir, path, store)
    }

    fn flow_of(store: &Store, caller: Caller, name: &str) -> Flow {
        let mut flow = Flow::new(name, caller.actor());
        flow.owner = caller.owner();
        store.insert_flow(&flow).expect("insert flow");
        flow
    }

    fn goal_of(store: &Store, caller: Caller, objective: &str) -> Goal {
        let mut goal = Goal::new(objective, caller.actor());
        goal.owner = caller.owner();
        store.insert_goal(&goal).expect("insert goal");
        goal
    }

    fn general_id(store: &Store) -> FlowId {
        store
            .conn
            .query_row("SELECT id FROM flows WHERE name = 'General'", [], |r| {
                r.get::<_, String>(0)
            })
            .map(FlowId::from_raw)
            .unwrap()
    }

    fn new_session(flow_id: Option<FlowId>, created_by: &str) -> Session {
        Session {
            id: SessionId::new(),
            flow_id,
            kind: "metadata".to_owned(),
            pid: None,
            cwd: None,
            state: SessionState::Running,
            presentation_state: SessionPresentationState::Detached,
            created_by: created_by.to_owned(),
            created_at: Timestamp::now(),
            owner: Owner::Unresolved,
        }
    }

    fn new_execution(goal_id: Option<GoalId>) -> Execution {
        Execution {
            id: ExecutionId::new(),
            agent_id: None,
            goal_id,
            reason: None,
            command: None,
            exit_code: None,
            duration_ms: None,
            started_at: Timestamp::now(),
            owner: Owner::Unresolved,
        }
    }

    fn foreground(store: &Store, caller: Caller) -> FlowView {
        store.foreground_flow_for(caller).expect("foreground")
    }

    fn rows(store: &Store) -> Vec<(i64, String)> {
        let mut stmt = store
            .conn
            .prepare("SELECT owner_uid, flow_id FROM flow_foreground ORDER BY owner_uid")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    /// The same NotFound text for a missing id and another owner's id.
    fn not_found_text(result: Result<impl std::fmt::Debug>) -> String {
        let error = result.expect_err("must be refused");
        assert_eq!(error.ipc_error_code(), ErrorCode::NotFound, "{error}");
        error.to_string()
    }

    #[test]
    fn two_uids_have_independent_foregrounds_on_separate_connections() {
        let (_dir, path, mut alice_conn) = file_store();
        let mut bob_conn = open(&path);
        let general = foreground(&alice_conn, ALICE).flow.id;
        let a = flow_of(&alice_conn, ALICE, "Alice work");
        let b = flow_of(&bob_conn, BOB, "Bob work");

        let view = alice_conn.activate_flow_for(ALICE, &a.id).unwrap();
        assert!(view.foreground);
        assert_eq!(view.flow.state, FlowState::Active, "lifecycle is unchanged");
        assert_eq!(foreground(&bob_conn, BOB).flow.id, general);

        bob_conn.activate_flow_for(BOB, &b.id).unwrap();
        assert_eq!(foreground(&alice_conn, ALICE).flow.id, a.id);
        assert_eq!(foreground(&alice_conn, BOB).flow.id, b.id);

        // Each sees only its own Flow plus General, with its own flag.
        let names = |store: &Store, caller| {
            store
                .flows_for(caller)
                .unwrap()
                .into_iter()
                .map(|view| (view.flow.name, view.foreground))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(&bob_conn, ALICE),
            [
                ("General".to_owned(), false),
                ("Alice work".to_owned(), true)
            ]
        );
        assert_eq!(
            names(&alice_conn, BOB),
            [("General".to_owned(), false), ("Bob work".to_owned(), true)]
        );
    }

    #[test]
    fn activating_general_only_removes_the_callers_row() {
        let (_dir, _path, mut store) = file_store();
        let a = flow_of(&store, ALICE, "A");
        let b = flow_of(&store, BOB, "B");
        store.activate_flow_for(ALICE, &a.id).unwrap();
        store.activate_flow_for(BOB, &b.id).unwrap();
        let general = store.ensure_general_flow().unwrap();
        let before = store.get_flow(&general).unwrap().unwrap();

        let view = store.activate_flow_for(BOB, &general).unwrap();
        assert!(view.foreground && view.is_general());
        assert_eq!(foreground(&store, ALICE).flow.id, a.id);
        assert_eq!(rows(&store), [(1001, a.id.to_string())]);
        // The shared General row itself is not modified by anyone's choice.
        assert_eq!(store.get_flow(&general).unwrap().unwrap(), before);
    }

    #[test]
    fn other_owners_and_unresolved_flows_cannot_be_seen_or_selected() {
        let (_dir, _path, mut store) = file_store();
        let alices = flow_of(&store, ALICE, "Private");
        let unresolved = Flow::new("Legacy", "user");
        store.insert_flow(&unresolved).unwrap();
        let missing = FlowId::new();

        assert!(store.flow_for(BOB, &alices.id).unwrap().is_none());
        assert!(store.flow_for(BOB, &unresolved.id).unwrap().is_none());
        assert!(store.flow_for(ALICE, &unresolved.id).unwrap().is_none());
        let expected = |id: &FlowId| format!("record not found: flow {id}");
        assert_eq!(
            not_found_text(store.activate_flow_for(BOB, &alices.id)),
            expected(&alices.id)
        );
        assert_eq!(
            not_found_text(store.activate_flow_for(BOB, &missing)),
            expected(&missing)
        );
        assert_eq!(
            not_found_text(store.archive_flow_for(BOB, &alices.id)),
            expected(&alices.id)
        );
        not_found_text(store.activate_flow_for(ALICE, &unresolved.id));
        assert!(rows(&store).is_empty());
        assert_eq!(
            store.get_flow(&alices.id).unwrap().unwrap().state,
            FlowState::Active
        );
    }

    #[test]
    fn general_cannot_be_archived_and_archived_flows_cannot_be_selected() {
        let (_dir, _path, mut store) = file_store();
        let general = store.ensure_general_flow().unwrap();
        let error = store.archive_flow_for(ALICE, &general).unwrap_err();
        assert_eq!(error.ipc_error_code(), ErrorCode::PermissionDenied);
        let a = flow_of(&store, ALICE, "A");
        store.archive_flow_for(ALICE, &a.id).unwrap();
        let error = store.activate_flow_for(ALICE, &a.id).unwrap_err();
        assert_eq!(error.ipc_error_code(), ErrorCode::Conflict);
        assert!(rows(&store).is_empty());
    }

    #[test]
    fn archiving_the_foreground_falls_back_to_general_for_that_owner_only() {
        let (_dir, _path, mut store) = file_store();
        let a = flow_of(&store, ALICE, "A");
        let b = flow_of(&store, BOB, "B");
        store.activate_flow_for(ALICE, &a.id).unwrap();
        store.activate_flow_for(BOB, &b.id).unwrap();
        let archived = store.archive_flow_for(ALICE, &a.id).unwrap();
        assert_eq!(archived.flow.state, FlowState::Archived);
        assert!(!archived.foreground);
        assert!(foreground(&store, ALICE).is_general());
        assert_eq!(foreground(&store, BOB).flow.id, b.id);
    }

    #[test]
    fn database_rejects_dangling_or_foreign_foreground_rows_from_any_writer() {
        let (_dir, path, store) = file_store();
        let raw = Store::open(&path).unwrap();
        let general = general_id(&store);
        let a = flow_of(&store, ALICE, "A");
        let unresolved = Flow::new("Legacy", "user");
        store.insert_flow(&unresolved).unwrap();
        let archived = flow_of(&store, ALICE, "Old");
        store
            .set_flow_state(&archived.id, FlowState::Archived)
            .unwrap();
        let insert = |uid: u32, flow: &str| {
            raw.conn.execute(
                "INSERT INTO flow_foreground (owner_uid, flow_id, activated_at) VALUES (?1, ?2, 1)",
                params![uid, flow],
            )
        };
        assert!(insert(1002, a.id.as_str()).is_err(), "other owner");
        assert!(insert(1001, general.as_str()).is_err(), "General is no row");
        assert!(insert(1001, unresolved.id.as_str()).is_err(), "unresolved");
        assert!(insert(1001, archived.id.as_str()).is_err(), "archived");
        assert!(insert(1001, "flow-missing").is_err(), "missing");
        assert!(insert(u32::MAX, a.id.as_str()).is_err(), "invalid uid");
        insert(1001, a.id.as_str()).expect("own unarchived Flow");
        assert!(raw
            .conn
            .execute(
                "UPDATE flow_foreground SET owner_uid = 1002 WHERE owner_uid = 1001",
                [],
            )
            .is_err());
        assert_eq!(rows(&store), [(1001, a.id.to_string())]);
    }

    #[test]
    fn ownership_transfer_and_delete_leave_no_dangling_foreground() {
        let (_dir, _path, mut store) = file_store();
        let a = flow_of(&store, ALICE, "A");
        let c = flow_of(&store, ALICE, "C");
        store.activate_flow_for(ALICE, &a.id).unwrap();
        // Transfer, as a future audited Admin claim/release would do.
        store
            .conn
            .execute(
                "UPDATE flows SET owner_uid = 1002 WHERE id = ?1",
                params![a.id.as_str()],
            )
            .unwrap();
        assert!(rows(&store).is_empty());
        assert!(foreground(&store, ALICE).is_general());
        assert!(foreground(&store, BOB).is_general(), "not inherited");

        store.activate_flow_for(ALICE, &c.id).unwrap();
        store
            .conn
            .execute("DELETE FROM flows WHERE id = ?1", params![c.id.as_str()])
            .unwrap();
        assert!(rows(&store).is_empty());
        assert!(foreground(&store, ALICE).is_general());
    }

    #[test]
    fn foreground_survives_restart() {
        let (_dir, path, mut store) = file_store();
        let a = flow_of(&store, ALICE, "A");
        store.activate_flow_for(ALICE, &a.id).unwrap();
        drop(store);
        let reopened = open(&path);
        assert_eq!(foreground(&reopened, ALICE).flow.id, a.id);
        assert!(foreground(&reopened, BOB).is_general());
    }

    #[test]
    fn concurrent_same_uid_activations_leave_exactly_one_row() {
        let (_dir, path, store) = file_store();
        let flows: Vec<_> = (0..4)
            .map(|n| flow_of(&store, ALICE, &format!("F{n}")).id)
            .collect();
        let barrier = Arc::new(Barrier::new(flows.len()));
        let handles: Vec<_> = flows
            .iter()
            .cloned()
            .map(|id| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let mut store = Store::open(path).unwrap();
                    barrier.wait();
                    for _ in 0..10 {
                        store.activate_flow_for(ALICE, &id).unwrap();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let rows = rows(&store);
        assert_eq!(rows.len(), 1);
        assert!(flows.iter().any(|id| id.as_str() == rows[0].1));
    }

    #[test]
    fn concurrent_two_uid_activations_do_not_interfere() {
        let (_dir, path, store) = file_store();
        let a = flow_of(&store, ALICE, "A").id;
        let b = flow_of(&store, BOB, "B").id;
        let general = general_id(&store);
        let barrier = Arc::new(Barrier::new(2));
        let spawn = |caller: Caller, mine: FlowId| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            let general = general.clone();
            std::thread::spawn(move || {
                let mut store = Store::open(path).unwrap();
                barrier.wait();
                for n in 0..20 {
                    let target = if n % 2 == 0 { &mine } else { &general };
                    store.activate_flow_for(caller, target).unwrap();
                }
                store.activate_flow_for(caller, &mine).unwrap();
            })
        };
        let handles = [spawn(ALICE, a.clone()), spawn(BOB, b.clone())];
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(rows(&store), [(1001, a.to_string()), (1002, b.to_string())]);
    }

    #[test]
    fn archive_and_activate_race_never_leaves_an_archived_foreground() {
        for _ in 0..10 {
            let (_dir, path, store) = file_store();
            let a = flow_of(&store, ALICE, "A").id;
            let barrier = Arc::new(Barrier::new(2));
            let activate = {
                let (path, barrier, a) = (path.clone(), Arc::clone(&barrier), a.clone());
                std::thread::spawn(move || {
                    let mut store = Store::open(path).unwrap();
                    barrier.wait();
                    for _ in 0..20 {
                        if let Err(error) = store.activate_flow_for(ALICE, &a) {
                            assert_eq!(error.ipc_error_code(), ErrorCode::Conflict, "{error}");
                        }
                    }
                })
            };
            let archive = {
                let (path, barrier, a) = (path.clone(), Arc::clone(&barrier), a.clone());
                std::thread::spawn(move || {
                    let mut store = Store::open(path).unwrap();
                    barrier.wait();
                    store.archive_flow_for(ALICE, &a).unwrap();
                })
            };
            activate.join().unwrap();
            archive.join().unwrap();
            assert_eq!(
                store.get_flow(&a).unwrap().unwrap().state,
                FlowState::Archived
            );
            assert!(rows(&store).is_empty(), "archived Flow left as foreground");
            assert!(foreground(&store, ALICE).is_general());
        }
    }

    #[test]
    fn goals_sessions_and_executions_are_private_to_their_owner() {
        let (_dir, _path, mut store) = file_store();
        let goal = goal_of(&store, ALICE, "secret");
        assert!(store.goal_for(BOB, &goal.id).unwrap().is_none());
        assert!(store.goals_for(BOB).unwrap().is_empty());
        assert_eq!(store.goals_for(ALICE).unwrap(), std::slice::from_ref(&goal));
        let missing = GoalId::new();
        assert_eq!(
            not_found_text(store.set_goal_status_for(BOB, &goal.id, GoalStatus::Abandoned)),
            format!("record not found: goal {}", goal.id)
        );
        assert_eq!(
            not_found_text(store.set_goal_status_for(BOB, &missing, GoalStatus::Abandoned)),
            format!("record not found: goal {missing}")
        );
        assert_eq!(
            store.get_goal(&goal.id).unwrap().unwrap().status,
            goal.status
        );

        let mut session = new_session(None, "uid:1001");
        session.owner = ALICE.owner();
        store.insert_session(&session).unwrap();
        assert!(store.session_for(BOB, &session.id).unwrap().is_none());
        assert!(store.sessions_for(BOB).unwrap().is_empty());
        not_found_text(store.terminate_session_for(BOB, &session.id));
        assert_eq!(
            store.get_session(&session.id).unwrap().unwrap().state,
            session.state
        );
        let was = store.terminate_session_for(ALICE, &session.id).unwrap();
        assert_eq!(was.state, session.state);

        let mut execution = new_execution(Some(goal.id.clone()));
        execution.owner = ALICE.owner();
        store.insert_execution(&execution).unwrap();
        assert!(store.execution_for(BOB, &execution.id).unwrap().is_none());
        assert!(store.executions_for(BOB).unwrap().is_empty());
        assert_eq!(store.executions_for(ALICE).unwrap().len(), 1);

        let unresolved = Goal::new("legacy", "user");
        store.insert_goal(&unresolved).unwrap();
        assert!(store.goal_for(ALICE, &unresolved.id).unwrap().is_none());
    }

    #[test]
    fn cross_owner_references_are_refused_in_the_inserting_transaction() {
        let (_dir, _path, mut store) = file_store();
        let alices_goal = goal_of(&store, ALICE, "a");
        let alices_flow = flow_of(&store, ALICE, "A");
        let general = general_id(&store);

        let mut child = Goal::new("child", "uid:1002");
        child.owner = BOB.owner();
        child.parent_goal = Some(alices_goal.id.clone());
        not_found_text(store.insert_goal_with_provenance(&child, &[]));

        let mut session = new_session(Some(alices_flow.id.clone()), "uid:1002");
        session.owner = BOB.owner();
        not_found_text(store.insert_session_with_provenance(&session, &[]));
        session.flow_id = Some(general.clone());
        store
            .insert_session_with_provenance(&session, &[])
            .expect("General is usable by everyone");

        let mut execution = new_execution(Some(alices_goal.id.clone()));
        execution.owner = BOB.owner();
        not_found_text(store.insert_execution_with_provenance(&execution, &[]));

        // The database refuses the same references from any other writer.
        child.id = GoalId::new();
        assert!(store.insert_goal(&child).is_err());
        let mut other = new_session(Some(alices_flow.id.clone()), "uid:1002");
        other.owner = BOB.owner();
        assert!(store.insert_session(&other).is_err());
        execution.id = ExecutionId::new();
        assert!(store.insert_execution(&execution).is_err());
        assert_eq!(store.goals_for(BOB).unwrap().len(), 0);
        assert_eq!(store.executions_for(BOB).unwrap().len(), 0);
    }

    #[test]
    fn status_counts_are_scoped_for_the_enforced_families() {
        let (_dir, _path, store) = file_store();
        flow_of(&store, ALICE, "A");
        let goal = goal_of(&store, ALICE, "g");
        let mut execution = new_execution(Some(goal.id));
        execution.owner = ALICE.owner();
        store.insert_execution(&execution).unwrap();
        let alice = store.status_counts_for(ALICE).unwrap();
        let bob = store.status_counts_for(BOB).unwrap();
        assert_eq!((alice.flows, alice.goals, alice.executions), (2, 1, 1));
        assert_eq!(
            (bob.flows, bob.goals, bob.executions, bob.sessions),
            (1, 0, 0, 0)
        );
    }

    fn edge(store: &Store, from: ObjectRef, to: ObjectRef) {
        store
            .insert_provenance(&ProvenanceEdge {
                from,
                relation: crate::types::ProvenanceRelation::BelongsTo,
                to,
                created_at: Timestamp::now(),
                note: None,
            })
            .unwrap();
    }

    #[test]
    fn provenance_requires_both_endpoints_visible() {
        let (_dir, _path, store) = file_store();
        let general = general_id(&store);
        let mine = goal_of(&store, ALICE, "mine");
        let theirs = goal_of(&store, BOB, "theirs");
        let legacy = Goal::new("legacy", "user");
        store.insert_goal(&legacy).unwrap();
        let agent = ObjectRef::from(&crate::types::AgentId::new());
        let model = ObjectRef::from(&crate::types::ModelId::from_raw("model-shared"));
        let g = |id: &GoalId| ObjectRef::from(id);
        edge(&store, g(&mine.id), ObjectRef::from(&general));
        edge(&store, g(&mine.id), g(&theirs.id));
        edge(&store, g(&mine.id), g(&legacy.id));
        edge(&store, g(&mine.id), agent.clone());
        edge(&store, g(&mine.id), model.clone());
        edge(&store, g(&mine.id), ObjectRef::actor("uid:1001").unwrap());
        edge(&store, g(&theirs.id), ObjectRef::actor("uid:1002").unwrap());
        edge(&store, agent.clone(), ObjectRef::actor("planner").unwrap());

        let targets = |caller, from: &ObjectRef| -> Vec<String> {
            store
                .provenance_from_for(caller, from)
                .unwrap()
                .into_iter()
                .map(|e| e.to.as_str().to_owned())
                .collect()
        };
        assert_eq!(
            targets(ALICE, &g(&mine.id)),
            [
                general.to_string(),
                agent.as_str().to_owned(),
                model.as_str().to_owned(),
                "actor:uid:1001".to_owned()
            ]
        );
        // Bob sees nothing of alice's Goal, and not the edge into his own.
        assert!(targets(BOB, &g(&mine.id)).is_empty());
        assert!(store
            .provenance_to_for(BOB, &g(&theirs.id))
            .unwrap()
            .is_empty());
        assert!(store
            .provenance_to_for(ALICE, &ObjectRef::actor("uid:1002").unwrap())
            .unwrap()
            .is_empty());
        // Neutral to neutral is never shown.
        assert!(targets(ALICE, &agent).is_empty());
        // Status counts the same edges the queries show, plus the shared
        // General bootstrap edge (General -> actor:system) everyone sees.
        assert_eq!(store.status_counts_for(ALICE).unwrap().provenance_edges, 5);
        assert_eq!(store.status_counts_for(BOB).unwrap().provenance_edges, 2);
    }

    #[test]
    fn status_counts_every_owned_family_per_caller() {
        let (_dir, _path, store) = file_store();
        let insert = |sql: &str| store.conn.execute_batch(sql).unwrap();
        insert(
            "INSERT INTO generations (id, status, created_at, owner_class, owner_uid) VALUES
               ('gen-a', 'CANDIDATE', 1, 'user', 1001),
               ('gen-b', 'CANDIDATE', 1, 'user', 1002),
               ('gen-s', 'STABLE', 1, 'system', NULL),
               ('gen-u', 'CANDIDATE', 1, 'unresolved', NULL);
             INSERT INTO ai_profiles (id, name, created_by, created_at, owner_class, owner_uid)
             VALUES ('profile-a', 'a', 'uid:1001', 1, 'user', 1001),
                    ('profile-b', 'b', 'uid:1002', 1, 'user', 1002);",
        );
        let alice = store.status_counts_for(ALICE).unwrap();
        let bob = store.status_counts_for(BOB).unwrap();
        assert_eq!((alice.generations, alice.profiles), (2, 1));
        assert_eq!((bob.generations, bob.profiles), (2, 1));
        assert_eq!(store.status_counts().unwrap().generations, 4);
    }

    #[test]
    fn generation_reason_goal_must_have_the_same_owner() {
        let (_dir, _path, mut store) = file_store();
        let theirs = goal_of(&store, BOB, "theirs");
        let mine = goal_of(&store, ALICE, "mine");
        let generation = |goal: &GoalId| crate::types::Generation {
            id: crate::types::GenerationId::new(),
            parent: None,
            source_commit: None,
            system_manifest: None,
            reason_goal: Some(goal.clone()),
            experiment: None,
            status: crate::types::GenerationStatus::Candidate,
            created_at: Timestamp::now(),
            owner: ALICE.owner(),
        };
        not_found_text(store.insert_generation_with_provenance(&generation(&theirs.id), &[]));
        assert!(
            store.insert_generation(&generation(&theirs.id)).is_err(),
            "trigger"
        );
        store
            .insert_generation_with_provenance(&generation(&mine.id), &[])
            .expect("own Goal");
    }

    /// A database at schema 9 (before per-owner foreground), as an upgraded
    /// installation has it.
    fn schema_nine(path: &Path) -> Store {
        let store = Store::open(path).unwrap();
        store
            .conn
            .execute_batch(
                "CREATE TABLE schema_migrations (
                     version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at INTEGER NOT NULL)",
            )
            .unwrap();
        for migration in MIGRATIONS.iter().filter(|m| m.version <= 9) {
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

    fn legacy_flow(
        store: &Store,
        id: &str,
        name: &str,
        state: &str,
        class: &str,
        uid: Option<i64>,
    ) {
        store
            .conn
            .execute(
                "INSERT INTO flows (id, name, state, created_by, created_at, owner_class, owner_uid)
                 VALUES (?1, ?2, ?3, 'x', 1, ?4, ?5)",
                params![id, name, state, class, uid],
            )
            .unwrap();
    }

    fn version(store: &Store) -> i64 {
        store
            .conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })
            .unwrap()
    }

    #[test]
    fn migration_carries_a_user_foreground_to_its_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let mut store = schema_nine(&path);
        legacy_flow(&store, "flow-general", "General", "ACTIVE", "system", None);
        legacy_flow(&store, "flow-a", "A", "FOREGROUND", "user", Some(1001));
        store.migrate().unwrap();
        assert_eq!(Some(version(&store)), MIGRATIONS.last().map(|m| m.version));
        assert_eq!(rows(&store), [(1001, "flow-a".to_owned())]);
        assert_eq!(foreground(&store, ALICE).flow.id.as_str(), "flow-a");
        assert!(foreground(&store, BOB).is_general());
        let a = store
            .get_flow(&FlowId::from_raw("flow-a"))
            .unwrap()
            .unwrap();
        assert_eq!(a.state, FlowState::Active);
    }

    #[test]
    fn migration_turns_system_or_unresolved_foregrounds_into_general_for_everyone() {
        for (id, name, class, uid) in [
            ("flow-general", "General", "system", None),
            ("flow-l", "Legacy", "unresolved", None),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut store = schema_nine(&dir.path().join("state.db"));
            if name != "General" {
                legacy_flow(&store, "flow-general", "General", "ACTIVE", "system", None);
            }
            legacy_flow(&store, id, name, "FOREGROUND", class, uid);
            store.migrate().unwrap();
            assert!(rows(&store).is_empty(), "{name}");
            assert!(foreground(&store, ALICE).is_general(), "{name}");
            let state: String = store
                .conn
                .query_row("SELECT state FROM flows WHERE id = ?1", [id], |r| r.get(0))
                .unwrap();
            assert_eq!(state, "ACTIVE");
        }
    }

    #[test]
    fn failed_foreground_migration_leaves_schema_nine_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let mut store = schema_nine(&path);
        legacy_flow(&store, "flow-a", "A", "FOREGROUND", "user", Some(1001));
        // Sabotage a statement after DROP INDEX: the trigger name is taken.
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER flows_reject_foreground_insert AFTER INSERT ON goals
                 BEGIN SELECT 1; END;",
            )
            .unwrap();
        assert!(store.migrate().is_err());
        assert_eq!(version(&store), 9);
        let exists = |name: &str| -> bool {
            store
                .conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = ?1)",
                    [name],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert!(exists("idx_flows_single_foreground"));
        assert!(!exists("flow_foreground"));
        assert!(!exists("goals_parent_same_owner"));
        let state: String = store
            .conn
            .query_row("SELECT state FROM flows WHERE id = 'flow-a'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "FOREGROUND");

        // Once the obstacle is gone, the same database migrates normally.
        store
            .conn
            .execute_batch("DROP TRIGGER flows_reject_foreground_insert")
            .unwrap();
        store.migrate().unwrap();
        assert_eq!(rows(&store), [(1001, "flow-a".to_owned())]);
    }
}
