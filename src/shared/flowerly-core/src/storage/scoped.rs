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
//!
//! Compute (PR 5): profiles are the caller's own plus read-only `system`
//! profiles, with names unique per owner (migration 0012); workloads and
//! usage are the caller's own only. The model catalog and the provider
//! registry stay shared system data.

use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};

use super::Store;
use super::{
    append_profile_version_row, insert_provenance_row, map_execution, map_flow, map_goal,
    map_owner, map_provenance, map_session, map_workload, parse_text, MutationOperation, Result,
    StatusCounts, StorageError,
};
use crate::types::{
    Caller, Execution, ExecutionId, Flow, FlowId, Goal, GoalId, GoalStatus, ModelId, ObjectRef,
    Owner, ProfileId, ProvenanceEdge, Session, SessionId, SessionPresentationState, SessionState,
    Timestamp, WorkloadId, WorkloadRecord, WorkloadState,
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
///   Generation, profile or workload; General; a `system` Generation or
///   profile; a shared catalog model or provider; the caller's own
///   `actor:uid:<n>` label.
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
            WHEN {column} GLOB 'work-*' THEN {workloads}
            WHEN {column} GLOB 'model-*' OR {column} GLOB 'provider-*' THEN 1
            WHEN {column} GLOB 'actor:uid:*' THEN {column} = 'actor:uid:' || ?1
            ELSE NULL
         END)",
        goals = own("goals"),
        sessions = own("sessions"),
        executions = own("executions"),
        generations = own_or_system("generations"),
        profiles = own_or_system("ai_profiles"),
        workloads = own("inference_workloads"),
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
    /// Generations and profiles count the caller's own and `system` rows,
    /// active workloads only the caller's own; provenance counts edges
    /// visible by both endpoints.
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
                      WHERE state IN ('QUEUED', 'ADMITTED') AND {OWNED})",
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

/// A profile row as one caller sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileView {
    pub id: ProfileId,
    pub name: String,
    pub created_by: String,
    pub created_at: i64,
    pub owner: Owner,
}

/// Profiles visible to `?1`: its own and `system` ones.
const PROFILE_VISIBLE: &str =
    "((p.owner_class = 'user' AND p.owner_uid = ?1) OR p.owner_class = 'system')";
const PROFILE_COLUMNS: &str =
    "p.id, p.name, p.created_by, p.created_at, p.owner_class, p.owner_uid";
const WORKLOAD_COLUMNS: &str = "id, profile_id, profile_name, model_id, background, priority, \
     state, reason, ceilings, created_by, created_at, admitted_at, stopped_at, completed_at, \
     operation_id, owner_class, owner_uid";

fn map_profile_view(row: &Row<'_>) -> rusqlite::Result<ProfileView> {
    Ok(ProfileView {
        id: parse_text(row.get::<_, String>(0)?, 0)?,
        name: row.get(1)?,
        created_by: row.get(2)?,
        created_at: row.get(3)?,
        owner: map_owner(row)?,
    })
}

fn profile_view(conn: &Connection, caller: Caller, id: &ProfileId) -> Result<Option<ProfileView>> {
    conn.query_row(
        &format!(
            "SELECT {PROFILE_COLUMNS} FROM ai_profiles AS p WHERE {PROFILE_VISIBLE} AND p.id = ?2"
        ),
        params![caller.uid(), id.as_str()],
        map_profile_view,
    )
    .optional()
    .map_err(StorageError::from)
}

/// Only the caller's own profiles are mutable. A visible system profile is
/// read-only (`PermissionDenied`, which reveals nothing the caller cannot
/// already read); anything else is `NotFound`.
fn require_own_profile(conn: &Connection, caller: Caller, id: &ProfileId) -> Result<()> {
    match profile_view(conn, caller, id)? {
        Some(view) if view.owner == caller.owner() => Ok(()),
        Some(_) => Err(StorageError::PermissionDenied(
            "system profiles are read-only".to_owned(),
        )),
        None => Err(not_found("profile", id.as_str())),
    }
}

impl Store {
    /// Profiles visible to `caller` as `(id, name, latest version, created_at)`.
    pub fn profiles_for(&self, caller: Caller) -> Result<Vec<(ProfileId, String, u32, i64)>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT p.id, p.name, COALESCE(MAX(v.version), 0), p.created_at
             FROM ai_profiles AS p
             LEFT JOIN ai_profile_versions AS v ON v.profile_id = p.id
             WHERE {PROFILE_VISIBLE}
             GROUP BY p.id ORDER BY p.created_at, p.id"
        ))?;
        let rows = stmt.query_map(params![caller.uid()], |row| {
            Ok((
                parse_text::<ProfileId>(row.get::<_, String>(0)?, 0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn profile_for(&self, caller: Caller, id: &ProfileId) -> Result<Option<ProfileView>> {
        profile_view(&self.conn, caller, id)
    }

    /// The profile a child owned by `owner` names as its `base`: the owner's
    /// own profile of that name, else a system one. A system child resolves
    /// only system profiles, so no user's profile can enter a shared chain.
    pub fn profile_base_for(&self, owner: Owner, name: &str) -> Result<Option<ProfileView>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {PROFILE_COLUMNS} FROM ai_profiles AS p
                     WHERE p.name = ?1 AND (p.owner_class = 'system'
                         OR (p.owner_class = 'user' AND p.owner_class = ?2 AND p.owner_uid IS ?3))
                     ORDER BY p.owner_class = 'system' LIMIT 1"
                ),
                params![name, owner.class(), owner.uid()],
                map_profile_view,
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// `(toml, summary, created_by, created_at)` of a visible profile version.
    pub fn profile_version_for(
        &self,
        caller: Caller,
        id: &ProfileId,
        version: u32,
    ) -> Result<Option<(String, String, String, i64)>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT v.toml, v.summary, v.created_by, v.created_at
                     FROM ai_profile_versions AS v JOIN ai_profiles AS p ON p.id = v.profile_id
                     WHERE {PROFILE_VISIBLE} AND v.profile_id = ?2 AND v.version = ?3"
                ),
                params![caller.uid(), id.as_str(), version],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// Latest `(version, toml, summary)` of a visible profile.
    pub fn profile_latest_for(
        &self,
        caller: Caller,
        id: &ProfileId,
    ) -> Result<Option<(u32, String, String)>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT v.version, v.toml, v.summary
                     FROM ai_profile_versions AS v JOIN ai_profiles AS p ON p.id = v.profile_id
                     WHERE {PROFILE_VISIBLE} AND v.profile_id = ?2
                     ORDER BY v.version DESC LIMIT 1"
                ),
                params![caller.uid(), id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// Rename one of the caller's profiles. Names are unique per owner
    /// (migration 0012), so a clash reveals only the caller's own names.
    pub fn rename_profile_for(
        &mut self,
        caller: Caller,
        id: &ProfileId,
        new_name: &str,
        renamed_by: &str,
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_own_profile(&tx, caller, id)?;
        tx.execute(
            "UPDATE ai_profiles SET name = ?3
             WHERE id = ?2 AND owner_class = 'user' AND owner_uid = ?1",
            params![caller.uid(), id.as_str(), new_name],
        )?;
        insert_provenance_row(
            &tx,
            &ProvenanceEdge {
                from: ObjectRef::from(id),
                relation: crate::types::ProvenanceRelation::CausedBy,
                to: ObjectRef::actor(format!("rename:{renamed_by}")).map_err(|error| {
                    StorageError::Sqlite(rusqlite::Error::InvalidParameterName(error.to_string()))
                })?,
                created_at: Timestamp::now(),
                note: Some("profile rename".to_owned()),
            },
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Append a version to one of the caller's profiles.
    pub fn append_profile_version_for(
        &mut self,
        caller: Caller,
        id: &ProfileId,
        toml: &str,
        summary: &str,
        appended_by: &str,
        edges: &[ProvenanceEdge],
    ) -> Result<u32> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_own_profile(&tx, caller, id)?;
        let next = append_profile_version_row(&tx, id, toml, summary, appended_by, edges)?;
        tx.commit()?;
        Ok(next)
    }

    /// Idempotent [`Store::append_profile_version_for`]: an exact retry of
    /// `operation` replays the stored response instead of appending again.
    #[allow(clippy::too_many_arguments)]
    pub fn append_profile_version_once_for(
        &mut self,
        caller: Caller,
        id: &ProfileId,
        toml: &str,
        summary: &str,
        appended_by: &str,
        edges: &[ProvenanceEdge],
        operation: &MutationOperation<'_>,
        response: impl FnOnce(u32) -> String,
    ) -> Result<String> {
        self.run_once(operation, |conn| {
            require_own_profile(conn, caller, id)?;
            let next = append_profile_version_row(conn, id, toml, summary, appended_by, edges)?;
            Ok(response(next))
        })
    }

    /// The caller's own workloads, highest priority first.
    pub fn workloads_for(&self, caller: Caller) -> Result<Vec<WorkloadRecord>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {WORKLOAD_COLUMNS} FROM inference_workloads WHERE {OWNED}
             ORDER BY priority DESC, created_at"
        ))?;
        let rows = stmt.query_map(params![caller.uid()], map_workload)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn workload_for(&self, caller: Caller, id: &WorkloadId) -> Result<Option<WorkloadRecord>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {WORKLOAD_COLUMNS} FROM inference_workloads WHERE {OWNED} AND id = ?2"
                ),
                params![caller.uid(), id.as_str()],
                map_workload,
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// The caller's queued or admitted workloads of one profile, for the
    /// profile's concurrency ceiling.
    pub fn count_active_workloads_for(&self, caller: Caller, profile: &ProfileId) -> Result<u32> {
        self.conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM inference_workloads
                     WHERE {OWNED} AND profile_id = ?2 AND state IN ('QUEUED', 'ADMITTED')"
                ),
                params![caller.uid(), profile.as_str()],
                |row| row.get(0),
            )
            .map_err(StorageError::from)
    }

    /// Stop or complete one of the caller's running workloads. Another
    /// uid's or a missing workload is `NotFound`; one that is not running is
    /// a conflict.
    pub fn transition_workload_for(
        &mut self,
        caller: Caller,
        id: &WorkloadId,
        target: WorkloadState,
        edges: &[ProvenanceEdge],
    ) -> Result<()> {
        let column = match target {
            WorkloadState::Stopped => "stopped_at",
            WorkloadState::Completed => "completed_at",
            other => {
                return Err(StorageError::InvalidOperation(format!(
                    "invalid terminal workload transition to {other}"
                )));
            }
        };
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state: Option<String> = tx
            .query_row(
                &format!("SELECT state FROM inference_workloads WHERE {OWNED} AND id = ?2"),
                params![caller.uid(), id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        match state.as_deref() {
            None => return Err(not_found("workload", id.as_str())),
            Some("QUEUED" | "ADMITTED") => {}
            Some(_) => {
                return Err(StorageError::OperationConflict(format!(
                    "workload {id} is not running for a {target} transition"
                )));
            }
        }
        tx.execute(
            &format!(
                "UPDATE inference_workloads SET state = ?3, {column} = ?4
                 WHERE {OWNED} AND id = ?2"
            ),
            params![
                caller.uid(),
                id.as_str(),
                target.to_string(),
                Timestamp::now().as_millis()
            ],
        )?;
        for edge in edges {
            insert_provenance_row(&tx, edge)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The caller's own usage totals `(tokens_in, tokens_out, cost_micros)`
    /// for one model.
    pub fn usage_totals_for(&self, caller: Caller, model: &ModelId) -> Result<(u64, u64, u64)> {
        self.conn
            .query_row(
                &format!(
                    "SELECT COALESCE(SUM(tokens_in), 0), COALESCE(SUM(tokens_out), 0),
                            COALESCE(SUM(cost_micros), 0)
                     FROM inference_usage WHERE {OWNED} AND model_id = ?2"
                ),
                params![caller.uid(), model.as_str()],
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

    fn raw_profile(store: &Store, id: &str, name: &str, owner: Owner) {
        store
            .conn
            .execute(
                "INSERT INTO ai_profiles (id, name, created_by, created_at, owner_class, owner_uid)
                 VALUES (?1, ?2, 'test', 0, ?3, ?4)",
                params![id, name, owner.class(), owner.uid()],
            )
            .unwrap();
    }

    fn raw_workload(
        store: &Store,
        id: &str,
        profile: &str,
        owner: Owner,
    ) -> rusqlite::Result<usize> {
        store.conn.execute(
            "INSERT INTO inference_workloads
                 (id, profile_id, profile_name, model_id, background, priority, state, reason,
                  ceilings, created_by, created_at, owner_class, owner_uid)
             VALUES (?1, ?2, 'p', 'model-x', 'normal', 0, 'ADMITTED', 'r', '{}', 'test', 0, ?3, ?4)",
            params![id, profile, owner.class(), owner.uid()],
        )
    }

    fn raw_usage(store: &Store, workload: &str, owner: Owner) -> rusqlite::Result<usize> {
        store.conn.execute(
            "INSERT INTO inference_usage
                 (id, workload_id, model_id, tokens_in, tokens_out, latency_ms, cost_micros,
                  created_by, created_at, owner_class, owner_uid)
             VALUES ('usage-' || hex(randomblob(8)), ?1, 'model-x', 1, 1, 1, 0, 'test', 0, ?2, ?3)",
            params![workload, owner.class(), owner.uid()],
        )
    }

    #[test]
    fn database_keeps_workloads_and_usage_with_their_owner() {
        let (_dir, _path, store) = file_store();
        raw_profile(&store, "profile-a", "p", ALICE.owner());
        raw_profile(&store, "profile-s", "p", Owner::System);
        // The same name for another owner is allowed; twice for one is not.
        raw_profile(&store, "profile-b", "p", BOB.owner());
        assert!(store
            .conn
            .execute(
                "INSERT INTO ai_profiles (id, name, created_by, created_at, owner_class, owner_uid)
                 VALUES ('profile-a2', 'p', 'test', 0, 'user', 1001)",
                [],
            )
            .is_err());

        raw_workload(&store, "work-a", "profile-a", ALICE.owner()).unwrap();
        raw_workload(&store, "work-s", "profile-s", BOB.owner()).unwrap();
        assert!(raw_workload(&store, "work-x", "profile-a", BOB.owner()).is_err());
        assert!(raw_workload(&store, "work-y", "profile-none", ALICE.owner()).is_err());

        raw_usage(&store, "work-a", ALICE.owner()).unwrap();
        assert!(raw_usage(&store, "work-a", BOB.owner()).is_err());
        assert!(raw_usage(&store, "work-a", Owner::Unresolved).is_err());
        assert!(raw_usage(&store, "work-none", ALICE.owner()).is_err());

        let alice_totals = store
            .usage_totals_for(ALICE, &"model-x".parse().unwrap())
            .unwrap();
        let bob_totals = store
            .usage_totals_for(BOB, &"model-x".parse().unwrap())
            .unwrap();
        assert_eq!((alice_totals.0, bob_totals.0), (1, 0));
        assert_eq!(store.workloads_for(BOB).unwrap().len(), 1);
        assert!(store
            .workload_for(BOB, &"work-a".parse().unwrap())
            .unwrap()
            .is_none());

        // inference.request records provider -> workload; only the workload
        // owner may see it (regression: `work-` ids were once neutral).
        let provider = ObjectRef::try_new("provider-p").unwrap();
        let before = store.status_counts_for(BOB).unwrap().provenance_edges;
        edge(
            &store,
            provider.clone(),
            ObjectRef::try_new("work-a").unwrap(),
        );
        assert_eq!(
            store.provenance_from_for(ALICE, &provider).unwrap().len(),
            1
        );
        assert!(store
            .provenance_from_for(BOB, &provider)
            .unwrap()
            .is_empty());
        assert_eq!(
            store.status_counts_for(BOB).unwrap().provenance_edges,
            before
        );
    }

    #[test]
    fn profile_bases_resolve_own_first_and_system_children_only_to_system() {
        let (_dir, _path, store) = file_store();
        raw_profile(&store, "profile-own", "base", ALICE.owner());
        raw_profile(&store, "profile-sys", "base", Owner::System);
        raw_profile(&store, "profile-bob", "only-bob", BOB.owner());
        let found = |owner: Owner, name: &str| {
            store
                .profile_base_for(owner, name)
                .unwrap()
                .map(|view| view.id.as_str().to_owned())
        };
        assert_eq!(found(ALICE.owner(), "base").as_deref(), Some("profile-own"));
        assert_eq!(found(BOB.owner(), "base").as_deref(), Some("profile-sys"));
        assert_eq!(found(Owner::System, "base").as_deref(), Some("profile-sys"));
        assert_eq!(found(ALICE.owner(), "only-bob"), None);
        assert_eq!(found(Owner::System, "only-bob"), None);
        assert_eq!(found(Owner::Unresolved, "only-bob"), None);
    }

    #[test]
    fn system_profiles_are_read_only_and_foreign_ones_missing() {
        let (_dir, _path, mut store) = file_store();
        raw_profile(&store, "profile-sys", "shared", Owner::System);
        raw_profile(&store, "profile-a", "mine", ALICE.owner());
        let sys = "profile-sys".parse().unwrap();
        let alice = "profile-a".parse().unwrap();
        assert!(matches!(
            store.rename_profile_for(BOB, &sys, "x", "uid:1002"),
            Err(StorageError::PermissionDenied(_))
        ));
        assert!(matches!(
            store.append_profile_version_for(BOB, &sys, "x", "{}", "uid:1002", &[]),
            Err(StorageError::PermissionDenied(_))
        ));
        assert_eq!(
            not_found_text(store.rename_profile_for(BOB, &alice, "x", "uid:1002")),
            not_found_text(store.rename_profile_for(
                BOB,
                &"profile-none".parse().unwrap(),
                "x",
                "uid:1002"
            ))
            .replace("profile-none", "profile-a")
        );
        assert!(store.profile_for(BOB, &alice).unwrap().is_none());
        assert!(store.profile_latest_for(BOB, &alice).unwrap().is_none());
        assert_eq!(
            store.profile_for(ALICE, &alice).unwrap().unwrap().name,
            "mine"
        );
        store
            .rename_profile_for(ALICE, &alice, "renamed", "uid:1001")
            .unwrap();
        assert_eq!(store.profiles_for(BOB).unwrap().len(), 1, "only system");
        assert_eq!(store.profiles_for(ALICE).unwrap().len(), 2);
    }
}
