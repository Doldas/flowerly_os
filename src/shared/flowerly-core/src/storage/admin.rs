//! Audited Admin operations that cross or change ownership (#225).
//!
//! Each operation takes the real admin [`Caller`] and a required reason, and
//! writes one append-only `admin_audit` row (migration 0013) in the same
//! IMMEDIATE transaction as the change, so either both are committed or
//! neither is. Refusals are journaled by the daemon, not stored here.

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use super::{insert_provenance_row, map_generation, retry_on_busy, RetryConfig, Store};
use super::{Result, StorageError};
use crate::types::{
    Caller, Generation, GenerationId, GenerationStatus, ObjectRef, Owner, ProvenanceEdge,
    ProvenanceRelation, Timestamp,
};

/// Longest accepted audit reason, matching the 0013 `CHECK`.
pub const MAX_AUDIT_REASON: usize = 512;

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
    object: &ObjectRef,
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
            object.as_str(),
            from.class(),
            from.uid(),
            to.class(),
            to.uid(),
            reason,
        ],
    )?;
    Ok(())
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
                &object,
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

    /// Applied audited Admin operations on one object, oldest first.
    pub fn admin_audit_for_object(&self, object: &ObjectRef) -> Result<Vec<AdminAuditRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT actor_uid, command, object_ref, from_owner_class, from_owner_uid,
                    to_owner_class, to_owner_uid, reason, created_at
             FROM admin_audit WHERE object_ref = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![object.as_str()], |row| {
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
}
