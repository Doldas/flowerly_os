//! Audited Admin operations that cross or change ownership (#225).
//!
//! Each operation takes the real admin [`Caller`] and a required reason, and
//! writes one append-only `admin_audit` row (migration 0013) in the same
//! IMMEDIATE transaction as the change, so either both are committed or
//! neither is. Refusals are journaled by the daemon, not stored here.
//!
//! Ownership `claim` and `release` (#225 PR 7) move an *ownership
//! component*: the object and every object linked to it, transitively and
//! in both directions, by a same-owner reference (the 0010–0012 trigger
//! edges) while it has the same owner. Moving a single object would break
//! those invariants. `system` objects bound the component and never move.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use super::{insert_provenance_row, map_generation, retry_on_busy, RetryConfig, Store};
use super::{Result, StorageError};
use crate::types::{
    Caller, Generation, GenerationId, GenerationStatus, ObjectRef, Owner, ProvenanceEdge,
    ProvenanceRelation, Timestamp,
};

/// Longest accepted audit reason, matching the 0013 `CHECK`.
pub const MAX_AUDIT_REASON: usize = 512;

/// Largest ownership component one claim or release may move.
pub const MAX_OWNERSHIP_COMPONENT: usize = 10_000;

/// One applied audited Admin operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminAuditRecord {
    pub actor_uid: u32,
    pub command: String,
    pub object_ref: String,
    pub from_owner: Owner,
    pub to_owner: Owner,
    pub reason: String,
    pub created_at: i64,
}

fn validate_reason(reason: &str) -> Result<()> {
    if reason.trim().is_empty() || reason.len() > MAX_AUDIT_REASON {
        return Err(StorageError::InvalidOperation(format!(
            "reason must be 1..{MAX_AUDIT_REASON} bytes and not blank"
        )));
    }
    Ok(())
}

fn record_audit(
    conn: &Connection,
    admin: Caller,
    command: &str,
    object: &str,
    from: Owner,
    to: Owner,
    reason: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO admin_audit
             (created_at, actor_uid, command, object_ref, from_owner_class, from_owner_uid,
              to_owner_class, to_owner_uid, reason, outcome)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'applied')",
        params![
            Timestamp::now().as_millis(),
            admin.uid(),
            command,
            object,
            from.class(),
            from.uid(),
            to.class(),
            to.uid(),
            reason,
        ],
    )?;
    Ok(())
}

/// A stored object kind that carries an owner and can be claimed or released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnedKind {
    Flow,
    Goal,
    Session,
    Execution,
    Generation,
    Profile,
    Workload,
    Usage,
}

impl OwnedKind {
    const ALL: [(Self, &'static str); 8] = [
        (Self::Flow, "flow-"),
        (Self::Goal, "goal-"),
        (Self::Session, "session-"),
        (Self::Execution, "exec-"),
        (Self::Generation, "gen-"),
        (Self::Profile, "profile-"),
        (Self::Workload, "work-"),
        (Self::Usage, "usage-"),
    ];

    /// The kind of a typed id, from its prefix.
    fn of(id: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|(_, prefix)| id.starts_with(prefix))
            .map(|(kind, _)| *kind)
    }

    fn table(self) -> &'static str {
        match self {
            Self::Flow => "flows",
            Self::Goal => "goals",
            Self::Session => "sessions",
            Self::Execution => "executions",
            Self::Generation => "generations",
            Self::Profile => "ai_profiles",
            Self::Workload => "inference_workloads",
            Self::Usage => "inference_usage",
        }
    }

    /// Executions and Generations record no creator.
    fn has_created_by(self) -> bool {
        !matches!(self, Self::Execution | Self::Generation)
    }
}

/// Same-owner references `(from kind, column, to kind)`.
const OWNERSHIP_EDGES: [(OwnedKind, &str, OwnedKind); 8] = [
    (OwnedKind::Flow, "parent_flow", OwnedKind::Flow),
    (OwnedKind::Goal, "parent_goal", OwnedKind::Goal),
    (OwnedKind::Execution, "goal_id", OwnedKind::Goal),
    (OwnedKind::Session, "flow_id", OwnedKind::Flow),
    (OwnedKind::Generation, "reason_goal", OwnedKind::Goal),
    (OwnedKind::Generation, "parent", OwnedKind::Generation),
    (OwnedKind::Workload, "profile_id", OwnedKind::Profile),
    (OwnedKind::Usage, "workload_id", OwnedKind::Workload),
];

/// One object and its current owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedObject {
    pub object_ref: String,
    pub owner: Owner,
}

/// What `ownership.inspect` reports: ownership metadata only, never content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnershipReport {
    pub object_ref: String,
    pub owner: Owner,
    pub created_by: Option<String>,
    /// Objects that move together with this one, this one first. Empty for
    /// a `system` object, which never moves.
    pub component: Vec<OwnedObject>,
    /// Objects linked to the component that have another, non-system
    /// owner. A move is refused unless each has the target owner.
    pub foreign_links: Vec<OwnedObject>,
    /// Earlier applied audited operations on this object, oldest first.
    pub audit: Vec<AdminAuditRecord>,
}

/// Parse an owned object id; anything else is not claimable.
fn owned_ref(raw: &str) -> Result<OwnedKind> {
    let invalid = || {
        StorageError::InvalidOperation(format!(
            "`{raw}` is not a Flow, Goal, Session, Execution, Generation, profile, \
             workload or usage id"
        ))
    };
    let kind = OwnedKind::of(raw).ok_or_else(invalid)?;
    let suffix = raw.split_once('-').map_or("", |(_, rest)| rest);
    if suffix.is_empty()
        || !suffix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(invalid());
    }
    Ok(kind)
}

fn load_owner(conn: &Connection, kind: OwnedKind, id: &str) -> Result<Option<Owner>> {
    conn.query_row(
        &format!(
            "SELECT owner_class, owner_uid FROM {} WHERE id = ?1",
            kind.table()
        ),
        params![id],
        |row| {
            let class: String = row.get(0)?;
            Ok(Owner::from_columns(&class, row.get(1)?))
        },
    )
    .optional()
    .map_err(StorageError::from)
}

/// Existing objects referenced by, or referencing, `id`, with their owners.
/// Dangling references are skipped.
fn linked(conn: &Connection, kind: OwnedKind, id: &str) -> Result<Vec<(OwnedKind, String, Owner)>> {
    let mut found = Vec::new();
    for (from, column, to) in OWNERSHIP_EDGES {
        if from == kind {
            let target: Option<String> = conn
                .query_row(
                    &format!("SELECT {column} FROM {} WHERE id = ?1", from.table()),
                    params![id],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            if let Some(target) = target {
                if let Some(owner) = load_owner(conn, to, &target)? {
                    found.push((to, target, owner));
                }
            }
        }
        if to == kind {
            let mut stmt = conn.prepare(&format!(
                "SELECT id, owner_class, owner_uid FROM {} WHERE {column} = ?1 ORDER BY id",
                from.table()
            ))?;
            let rows = stmt.query_map(params![id], |row| {
                let class: String = row.get(1)?;
                Ok((from, row.get(0)?, Owner::from_columns(&class, row.get(2)?)))
            })?;
            for row in rows {
                found.push(row?);
            }
        }
    }
    Ok(found)
}

/// One component object: its kind and id.
type Member = (OwnedKind, String);

/// The ownership component of `id` (which has `owner`) and its foreign
/// links. Bounded by [`MAX_OWNERSHIP_COMPONENT`].
fn component(
    conn: &Connection,
    kind: OwnedKind,
    id: &str,
    owner: Owner,
) -> Result<(Vec<Member>, Vec<OwnedObject>)> {
    let mut members: Vec<Member> = vec![(kind, id.to_owned())];
    let mut seen = BTreeSet::from([id.to_owned()]);
    let mut foreign: BTreeMap<String, Owner> = BTreeMap::new();
    let mut queue = VecDeque::from([(kind, id.to_owned())]);
    while let Some((kind, id)) = queue.pop_front() {
        for (link_kind, link_id, link_owner) in linked(conn, kind, &id)? {
            if link_owner == Owner::System || seen.contains(&link_id) {
                continue;
            }
            if link_owner != owner {
                foreign.insert(link_id, link_owner);
                continue;
            }
            if members.len() == MAX_OWNERSHIP_COMPONENT {
                return Err(StorageError::InvalidOperation(format!(
                    "ownership component of {} exceeds {MAX_OWNERSHIP_COMPONENT} objects",
                    members[0].1
                )));
            }
            seen.insert(link_id.clone());
            members.push((link_kind, link_id.clone()));
            queue.push_back((link_kind, link_id));
        }
    }
    let foreign = foreign
        .into_iter()
        .map(|(object_ref, owner)| OwnedObject { object_ref, owner })
        .collect();
    Ok((members, foreign))
}

fn require_owned(conn: &Connection, raw: &str) -> Result<(OwnedKind, Owner)> {
    let kind = owned_ref(raw)?;
    let owner =
        load_owner(conn, kind, raw)?.ok_or_else(|| StorageError::NotFound(raw.to_owned()))?;
    Ok((kind, owner))
}

const GENERATION_COLUMNS: &str = "id, parent, source_commit, system_manifest, reason_goal, \
     experiment, status, created_at, owner_class, owner_uid";

fn load_generation(conn: &Connection, id: &GenerationId) -> Result<Option<Generation>> {
    conn.query_row(
        &format!("SELECT {GENERATION_COLUMNS} FROM generations WHERE id = ?1"),
        params![id.as_str()],
        map_generation,
    )
    .optional()
    .map_err(StorageError::from)
}

impl Store {
    /// Make a private (or `unresolved`) candidate a shared `system`
    /// Generation. Only `CANDIDATE` and `TESTED` Generations can be
    /// promoted. The audit row and a `CausedBy actor:uid:<admin>` edge are
    /// written in the same transaction. `reason_goal` is kept; the scoped
    /// reads show it only to the Goal's owner.
    pub fn promote_generation(
        &mut self,
        admin: Caller,
        id: &GenerationId,
        reason: &str,
    ) -> Result<Generation> {
        validate_reason(reason)?;
        retry_on_busy(RetryConfig::default(), || {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let generation = load_generation(&tx, id)?
                .ok_or_else(|| StorageError::NotFound(format!("generation {id}")))?;
            if generation.owner == Owner::System {
                return Err(StorageError::OperationConflict(format!(
                    "generation {id} is already a system Generation"
                )));
            }
            if !matches!(
                generation.status,
                GenerationStatus::Candidate | GenerationStatus::Tested
            ) {
                return Err(StorageError::OperationConflict(format!(
                    "generation {id} is {} (CANDIDATE or TESTED required)",
                    generation.status
                )));
            }
            tx.execute(
                "UPDATE generations SET owner_class = 'system', owner_uid = NULL WHERE id = ?1",
                params![id.as_str()],
            )?;
            let object = ObjectRef::from(id);
            record_audit(
                &tx,
                admin,
                "generation.promote",
                object.as_str(),
                generation.owner,
                Owner::System,
                reason,
            )?;
            insert_provenance_row(
                &tx,
                &ProvenanceEdge {
                    from: object,
                    relation: ProvenanceRelation::CausedBy,
                    to: ObjectRef::actor(admin.actor())
                        .map_err(|error| StorageError::InvalidOperation(error.to_string()))?,
                    created_at: Timestamp::now(),
                    note: Some("promoted to a system Generation".to_owned()),
                },
            )?;
            let promoted = load_generation(&tx, id)?
                .ok_or_else(|| StorageError::NotFound(format!("generation {id}")))?;
            tx.commit()?;
            Ok(promoted)
        })
    }

    /// Ownership metadata of one object, its component and foreign links,
    /// for an Admin. Reading across owners is itself audited: one row with
    /// the unchanged owner, in the same transaction as the read.
    pub fn inspect_ownership(
        &mut self,
        admin: Caller,
        object: &str,
        reason: &str,
    ) -> Result<OwnershipReport> {
        validate_reason(reason)?;
        retry_on_busy(RetryConfig::default(), || {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (kind, owner) = require_owned(&tx, object)?;
            let created_by = if kind.has_created_by() {
                tx.query_row(
                    &format!("SELECT created_by FROM {} WHERE id = ?1", kind.table()),
                    params![object],
                    |row| row.get(0),
                )?
            } else {
                None
            };
            let (component, foreign_links) = if owner == Owner::System {
                (Vec::new(), Vec::new())
            } else {
                let (members, foreign) = component(&tx, kind, object, owner)?;
                let members = members
                    .into_iter()
                    .map(|(_, object_ref)| OwnedObject { object_ref, owner })
                    .collect();
                (members, foreign)
            };
            let audit = audit_for(&tx, object)?;
            record_audit(
                &tx,
                admin,
                "ownership.inspect",
                object,
                owner,
                owner,
                reason,
            )?;
            tx.commit()?;
            Ok(OwnershipReport {
                object_ref: object.to_owned(),
                owner,
                created_by,
                component,
                foreign_links,
                audit,
            })
        })
    }

    /// Give an `unresolved` object, and its whole component, to `to_uid`.
    /// Returns the moved ids, the named object first.
    pub fn claim_ownership(
        &mut self,
        admin: Caller,
        object: &str,
        to_uid: u32,
        reason: &str,
        expect: usize,
    ) -> Result<Vec<String>> {
        if to_uid == u32::MAX {
            return Err(StorageError::InvalidOperation(
                "to_uid must be 0..4294967294".to_owned(),
            ));
        }
        self.move_component(
            admin,
            "ownership.claim",
            object,
            |owner| owner == Owner::Unresolved,
            Owner::User(to_uid),
            reason,
            expect,
        )
    }

    /// Make a user-owned object, and its whole component, `unresolved`
    /// (protected) again. Returns the moved ids, the named object first.
    pub fn release_ownership(
        &mut self,
        admin: Caller,
        object: &str,
        reason: &str,
        expect: usize,
    ) -> Result<Vec<String>> {
        self.move_component(
            admin,
            "ownership.release",
            object,
            |owner| matches!(owner, Owner::User(_)),
            Owner::Unresolved,
            reason,
            expect,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn move_component(
        &mut self,
        admin: Caller,
        command: &str,
        object: &str,
        movable: fn(Owner) -> bool,
        to: Owner,
        reason: &str,
        expect: usize,
    ) -> Result<Vec<String>> {
        validate_reason(reason)?;
        retry_on_busy(RetryConfig::default(), || {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (kind, from) = require_owned(&tx, object)?;
            if !movable(from) {
                return Err(StorageError::OperationConflict(format!(
                    "{command} does not apply to a {} object ({object})",
                    from.class()
                )));
            }
            let (members, foreign) = component(&tx, kind, object, from)?;
            if let Some(link) = foreign.iter().find(|link| link.owner != to) {
                return Err(StorageError::OperationConflict(format!(
                    "{object} is linked to {} owned by {}; move that first",
                    link.object_ref,
                    link.owner.class()
                )));
            }
            if members.len() != expect {
                return Err(StorageError::OperationConflict(format!(
                    "ownership component of {object} has {} objects, expected {expect}",
                    members.len()
                )));
            }
            for (kind, id) in &members {
                tx.execute(
                    &format!(
                        "UPDATE {} SET owner_class = ?1, owner_uid = ?2 WHERE id = ?3",
                        kind.table()
                    ),
                    params![to.class(), to.uid(), id],
                )?;
                record_audit(&tx, admin, command, id, from, to, reason)?;
            }
            // Post-condition, in the same transaction: no moved object has
            // a same-owner reference to or from another non-system owner.
            for (kind, id) in &members {
                if let Some((_, link, owner)) = linked(&tx, *kind, id)?
                    .into_iter()
                    .find(|(_, _, owner)| *owner != Owner::System && *owner != to)
                {
                    return Err(StorageError::OperationConflict(format!(
                        "{id} would reference {link} owned by {}",
                        owner.class()
                    )));
                }
            }
            tx.commit()?;
            Ok(members.into_iter().map(|(_, id)| id).collect())
        })
    }

    /// Applied audited Admin operations on one object, oldest first.
    pub fn admin_audit_for_object(&self, object: &ObjectRef) -> Result<Vec<AdminAuditRecord>> {
        audit_for(&self.conn, object.as_str())
    }
}

fn audit_for(conn: &Connection, object: &str) -> Result<Vec<AdminAuditRecord>> {
    let mut stmt = conn.prepare(
        "SELECT actor_uid, command, object_ref, from_owner_class, from_owner_uid,
                to_owner_class, to_owner_uid, reason, created_at
         FROM admin_audit WHERE object_ref = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map(params![object], |row| {
        let from_class: String = row.get(3)?;
        let to_class: String = row.get(5)?;
        Ok(AdminAuditRecord {
            actor_uid: row.get(0)?,
            command: row.get(1)?,
            object_ref: row.get(2)?,
            from_owner: Owner::from_columns(&from_class, row.get(4)?),
            to_owner: Owner::from_columns(&to_class, row.get(6)?),
            reason: row.get(7)?,
            created_at: row.get(8)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use rusqlite::params;

    use super::*;
    use crate::ipc::ErrorCode;
    use crate::types::GoalId;

    const ALICE: Caller = Caller::new(1001);
    const BOB: Caller = Caller::new(1002);
    const ADMIN: Caller = Caller::new(1500);

    fn store() -> Store {
        let mut store = Store::open_in_memory().expect("open");
        store.migrate().expect("migrate");
        store.ensure_general_flow().expect("General");
        store
    }

    fn general(store: &Store) -> String {
        store
            .conn
            .query_row("SELECT id FROM flows WHERE name = 'General'", [], |r| {
                r.get(0)
            })
            .unwrap()
    }

    /// A row as a pre-#225 daemon left it: no provable owner.
    fn legacy_flow(store: &Store, id: &str) {
        store
            .conn
            .execute(
                "INSERT INTO flows (id, name, state, created_by, created_at)
                 VALUES (?1, ?1, 'ACTIVE', 'user', 1)",
                params![id],
            )
            .unwrap();
    }

    fn goal(store: &Store, id: &str, parent: Option<&str>, owner: Owner) {
        store
            .conn
            .execute(
                "INSERT INTO goals (id, parent_goal, objective, status, created_by, created_at,
                                    owner_class, owner_uid)
                 VALUES (?1, ?2, 'secret objective', 'ACTIVE', 'user', 1, ?3, ?4)",
                params![id, parent, owner.class(), owner.uid()],
            )
            .unwrap();
    }

    fn legacy_session(store: &Store, id: &str, flow: &str) {
        store
            .conn
            .execute(
                "INSERT INTO sessions (id, flow_id, kind, state, created_by, created_at)
                 VALUES (?1, ?2, 'metadata', 'RUNNING', 'user', 1)",
                params![id, flow],
            )
            .unwrap();
    }

    fn legacy_execution(store: &Store, id: &str, goal: &str) {
        store
            .conn
            .execute(
                "INSERT INTO executions (id, goal_id, started_at) VALUES (?1, ?2, 1)",
                params![id, goal],
            )
            .unwrap();
    }

    fn owner_of(store: &Store, id: &str) -> Owner {
        load_owner(&store.conn, OwnedKind::of(id).unwrap(), id)
            .unwrap()
            .unwrap()
    }

    fn audit_count(store: &Store) -> i64 {
        store
            .conn
            .query_row("SELECT COUNT(*) FROM admin_audit", [], |r| r.get(0))
            .unwrap()
    }

    fn conflict<T: std::fmt::Debug>(result: Result<T>) -> String {
        match result {
            Err(error) if error.ipc_error_code() == ErrorCode::Conflict => error.to_string(),
            other => panic!("expected a conflict, got {other:?}"),
        }
    }

    #[test]
    fn claim_moves_the_whole_component_with_audit_and_release_reverses_it() {
        let mut store = store();
        goal(&store, "goal-root", None, Owner::Unresolved);
        goal(&store, "goal-child", Some("goal-root"), Owner::Unresolved);
        legacy_execution(&store, "exec-run", "goal-child");
        goal(&store, "goal-unrelated", None, Owner::Unresolved);

        let report = store
            .inspect_ownership(ADMIN, "goal-child", "triage legacy data")
            .unwrap();
        assert_eq!(report.owner, Owner::Unresolved);
        assert_eq!(report.created_by.as_deref(), Some("user"));
        let refs: Vec<_> = report
            .component
            .iter()
            .map(|o| o.object_ref.as_str())
            .collect();
        assert_eq!(refs, ["goal-child", "goal-root", "exec-run"]);
        assert!(report.foreign_links.is_empty());
        assert_eq!(audit_count(&store), 1, "inspect is audited");

        // A stale `expect` moves nothing and records nothing.
        conflict(store.claim_ownership(ADMIN, "goal-root", ALICE.uid(), "legacy", 2));
        assert_eq!(owner_of(&store, "goal-root"), Owner::Unresolved);
        assert_eq!(audit_count(&store), 1);

        let moved = store
            .claim_ownership(ADMIN, "goal-root", ALICE.uid(), "legacy Alice work", 3)
            .unwrap();
        assert_eq!(moved, ["goal-root", "goal-child", "exec-run"]);
        for id in &moved {
            assert_eq!(owner_of(&store, id), ALICE.owner(), "{id}");
        }
        assert_eq!(owner_of(&store, "goal-unrelated"), Owner::Unresolved);
        let root = GoalId::from_raw("goal-root");
        assert!(store.goal_for(ALICE, &root).unwrap().is_some());
        assert!(store.goal_for(BOB, &root).unwrap().is_none());

        let audit = store
            .admin_audit_for_object(&ObjectRef::try_new("exec-run").unwrap())
            .unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(
            (
                audit[0].actor_uid,
                audit[0].command.as_str(),
                audit[0].from_owner,
                audit[0].to_owner,
                audit[0].reason.as_str()
            ),
            (
                ADMIN.uid(),
                "ownership.claim",
                Owner::Unresolved,
                ALICE.owner(),
                "legacy Alice work"
            )
        );

        // Claiming again is a conflict: only `unresolved` can be claimed.
        conflict(store.claim_ownership(ADMIN, "goal-root", BOB.uid(), "steal", 3));

        let released = store
            .release_ownership(ADMIN, "exec-run", "wrong owner", 3)
            .unwrap();
        assert_eq!(released, ["exec-run", "goal-child", "goal-root"]);
        for id in &released {
            assert_eq!(owner_of(&store, id), Owner::Unresolved, "{id}");
        }
        assert!(store.goal_for(ALICE, &root).unwrap().is_none());
        conflict(store.release_ownership(ADMIN, "exec-run", "again", 3));
        assert_eq!(audit_count(&store), 1 + 3 + 3);
    }

    #[test]
    fn system_objects_never_move_and_general_bounds_the_component() {
        let mut store = store();
        let general = general(&store);
        legacy_session(&store, "session-legacy", &general);

        conflict(store.claim_ownership(ADMIN, &general, ALICE.uid(), "x", 1));
        conflict(store.release_ownership(ADMIN, &general, "x", 1));
        let report = store.inspect_ownership(ADMIN, &general, "look").unwrap();
        assert_eq!(report.owner, Owner::System);
        assert!(report.component.is_empty());

        // A legacy Session in General moves alone; General stays shared.
        let moved = store
            .claim_ownership(ADMIN, "session-legacy", ALICE.uid(), "legacy shell", 1)
            .unwrap();
        assert_eq!(moved, ["session-legacy"]);
        assert_eq!(owner_of(&store, &general), Owner::System);
    }

    #[test]
    fn a_link_to_another_owner_blocks_the_move_unless_it_is_the_target() {
        let mut store = store();
        goal(&store, "goal-alice", None, ALICE.owner());
        // A legacy child of Alice's Goal (the insert trigger predates it).
        goal(&store, "goal-legacy", None, Owner::Unresolved);
        store
            .conn
            .execute(
                "UPDATE goals SET parent_goal = 'goal-alice' WHERE id = 'goal-legacy'",
                [],
            )
            .unwrap();

        let report = store.inspect_ownership(ADMIN, "goal-legacy", "x").unwrap();
        assert_eq!(
            report.foreign_links,
            [OwnedObject {
                object_ref: "goal-alice".to_owned(),
                owner: ALICE.owner()
            }]
        );
        let message = conflict(store.claim_ownership(ADMIN, "goal-legacy", BOB.uid(), "x", 1));
        assert!(message.contains("goal-alice"), "{message}");
        assert_eq!(owner_of(&store, "goal-legacy"), Owner::Unresolved);

        store
            .claim_ownership(ADMIN, "goal-legacy", ALICE.uid(), "Alice's subgoal", 1)
            .unwrap();
        assert_eq!(owner_of(&store, "goal-legacy"), ALICE.owner());
        // Now one component: releasing either end releases both.
        let report = store.inspect_ownership(ADMIN, "goal-alice", "x").unwrap();
        assert_eq!(report.component.len(), 2);
    }

    #[test]
    fn release_drops_the_owner_foreground_and_claims_respect_profile_names() {
        let mut store = store();
        store
            .conn
            .execute(
                "INSERT INTO flows (id, name, state, created_by, created_at, owner_class, owner_uid)
                 VALUES ('flow-alice', 'mine', 'ACTIVE', 'uid:1001', 1, 'user', 1001)",
                [],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO flow_foreground (owner_uid, flow_id, activated_at)
                 VALUES (1001, 'flow-alice', 1)",
                [],
            )
            .unwrap();
        store
            .release_ownership(ADMIN, "flow-alice", "account removed", 1)
            .unwrap();
        let rows: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM flow_foreground", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0, "Alice falls back to General");

        for (id, class, uid) in [
            ("profile-alice", "user", Some(1001)),
            ("profile-legacy", "unresolved", None),
        ] {
            store
                .conn
                .execute(
                    "INSERT INTO ai_profiles (id, name, created_by, created_at, owner_class, owner_uid)
                     VALUES (?1, 'default', 'user', 1, ?2, ?3)",
                    params![id, class, uid],
                )
                .unwrap();
        }
        let before = audit_count(&store);
        conflict(store.claim_ownership(ADMIN, "profile-legacy", ALICE.uid(), "x", 1));
        assert_eq!(owner_of(&store, "profile-legacy"), Owner::Unresolved);
        assert_eq!(audit_count(&store), before, "nothing recorded");
        store
            .claim_ownership(ADMIN, "profile-legacy", BOB.uid(), "Bob's", 1)
            .unwrap();
    }

    #[test]
    fn malformed_requests_are_refused_before_anything_is_recorded() {
        let mut store = store();
        legacy_flow(&store, "flow-legacy");
        for raw in [
            "",
            "bogus",
            "model-x",
            "provider-x",
            "goal-",
            "goal-a b",
            "flow",
        ] {
            let error = store.inspect_ownership(ADMIN, raw, "x").unwrap_err();
            assert_eq!(error.ipc_error_code(), ErrorCode::InvalidArgument, "{raw}");
        }
        let missing = store
            .inspect_ownership(ADMIN, "goal-none", "x")
            .unwrap_err();
        assert_eq!(missing.ipc_error_code(), ErrorCode::NotFound);
        for reason in ["", "  ", &"r".repeat(MAX_AUDIT_REASON + 1)] {
            assert!(store
                .inspect_ownership(ADMIN, "flow-legacy", reason)
                .is_err());
            assert!(store
                .claim_ownership(ADMIN, "flow-legacy", 1001, reason, 1)
                .is_err());
        }
        assert!(store
            .claim_ownership(ADMIN, "flow-legacy", u32::MAX, "x", 1)
            .is_err());
        assert_eq!(audit_count(&store), 0);
        assert_eq!(owner_of(&store, "flow-legacy"), Owner::Unresolved);
    }
}
