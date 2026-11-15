//! Generation lifecycle and durable activation records (ADR-0023, G1).
//!
//! The rules are enforced by migration 0015 for every writer; this module is
//! the typed way to use them. Nothing here selects a root, talks to a broker or
//! reads the witness ledger: a caller (G3) decides that an activation is
//! confirmed and records it here. The database then refuses anything that is not
//! a legal transition, a second `STABLE`, a `STABLE` without a confirmed
//! activation, or a rewrite of history.

use std::str::FromStr;

use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::admin::{load_generation, record_audit, validate_reason};
use super::{insert_provenance_row, retry_on_busy, RetryConfig, Store};
use super::{Result, StorageError};
use crate::types::{
    Caller, ExperimentId, Generation, GenerationId, GenerationStatus, ObjectRef, Owner,
    ProvenanceEdge, ProvenanceRelation, Timestamp,
};
use crate::witness::{decide, Decision, Expectation, Ledger};

pub const MAX_OPERATION_ID: usize = 128;
const MAX_DETAIL: usize = 1024;
const ZERO_HEAD: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// The complete typed identity of a root: what a witnessed boot is compared to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootIdentity {
    pub rootfs_sha256: String,
    pub kernel_sha256: String,
    pub initramfs_sha256: String,
    pub input_manifest_sha256: String,
    pub build_id: String,
}

fn is_hex(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn invalid(message: impl Into<String>) -> StorageError {
    StorageError::InvalidOperation(message.into())
}

impl RootIdentity {
    pub fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("rootfs_sha256", &self.rootfs_sha256),
            ("kernel_sha256", &self.kernel_sha256),
            ("initramfs_sha256", &self.initramfs_sha256),
            ("input_manifest_sha256", &self.input_manifest_sha256),
        ] {
            if !is_hex(value, 64) {
                return Err(invalid(format!("{name} must be 64 lowercase hex digits")));
            }
        }
        if self.build_id.is_empty()
            || self.build_id.len() > 64
            || !self
                .build_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
        {
            return Err(invalid("build_id must be 1..64 of [A-Za-z0-9_.-]"));
        }
        Ok(())
    }
}

/// What an acceptance run concluded about a Generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceOutcome {
    Passed,
    Failed,
}

impl EvidenceOutcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
        }
    }
}

impl FromStr for EvidenceOutcome {
    type Err = String;

    fn from_str(text: &str) -> std::result::Result<Self, Self::Err> {
        match text {
            "passed" => Ok(Self::Passed),
            "failed" => Ok(Self::Failed),
            other => Err(format!("outcome must be passed or failed, not `{other}`")),
        }
    }
}

/// An idempotency key: 1..128 of `[A-Za-z0-9_.-]`.
pub fn validate_operation_id(key: &str) -> Result<()> {
    if key.is_empty()
        || key.len() > MAX_OPERATION_ID
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
    {
        return Err(invalid("operation_id must be 1..128 of [A-Za-z0-9_.-]"));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationKind {
    Activate,
    Rollback,
}

impl ActivationKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Activate => "ACTIVATE",
            Self::Rollback => "ROLLBACK",
        }
    }

    fn parse(text: &str) -> rusqlite::Result<Self> {
        match text {
            "ACTIVATE" => Ok(Self::Activate),
            "ROLLBACK" => Ok(Self::Rollback),
            _ => Err(corrupt(format!("unknown activation kind {text}"))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationState {
    Pending,
    Confirmed,
    FellBack,
    Abandoned,
}

impl ActivationState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Confirmed => "CONFIRMED",
            Self::FellBack => "FELL_BACK",
            Self::Abandoned => "ABANDONED",
        }
    }

    fn parse(text: &str) -> rusqlite::Result<Self> {
        match text {
            "PENDING" => Ok(Self::Pending),
            "CONFIRMED" => Ok(Self::Confirmed),
            "FELL_BACK" => Ok(Self::FellBack),
            "ABANDONED" => Ok(Self::Abandoned),
            _ => Err(corrupt(format!("unknown activation state {text}"))),
        }
    }
}

fn corrupt(message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, message.into())
}

/// A durable activation record (one row of the append-only history).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activation {
    pub id: i64,
    pub generation: GenerationId,
    pub previous_generation: Option<GenerationId>,
    pub kind: ActivationKind,
    pub actor_uid: u32,
    pub reason: String,
    pub operation_id: Option<String>,
    pub request_digest: String,
    pub expected_slot: String,
    pub expected_root_partuuid: String,
    pub ledger_sequence: u64,
    pub ledger_head: String,
    pub irreversible: bool,
    pub backup_sha256: Option<String>,
    pub acknowledges: Option<i64>,
    pub state: ActivationState,
    pub requested_at: Timestamp,
    pub resolved_at: Option<Timestamp>,
    pub witness_boot_id: Option<String>,
    pub witness_sequence: Option<u64>,
    pub detail: Option<String>,
}

const ACTIVATION_COLUMNS: &str = "id, generation, previous_generation, kind, actor_uid, reason, \
     operation_id, request_digest, expected_slot, expected_root_partuuid, ledger_sequence, \
     ledger_head, irreversible, backup_sha256, acknowledges, state, requested_at, resolved_at, \
     witness_boot_id, witness_sequence, detail";

fn map_activation(row: &Row<'_>) -> rusqlite::Result<Activation> {
    let parse_id = |raw: String, index: usize| -> rusqlite::Result<GenerationId> {
        raw.parse().map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                "invalid generation id".into(),
            )
        })
    };
    let previous: Option<String> = row.get(2)?;
    let kind: String = row.get(3)?;
    let state: String = row.get(15)?;
    Ok(Activation {
        id: row.get(0)?,
        generation: parse_id(row.get(1)?, 1)?,
        previous_generation: previous.map(|raw| parse_id(raw, 2)).transpose()?,
        kind: ActivationKind::parse(&kind)?,
        actor_uid: row.get(4)?,
        reason: row.get(5)?,
        operation_id: row.get(6)?,
        request_digest: row.get(7)?,
        expected_slot: row.get(8)?,
        expected_root_partuuid: row.get(9)?,
        ledger_sequence: row.get::<_, i64>(10)? as u64,
        ledger_head: row.get(11)?,
        irreversible: row.get::<_, i64>(12)? == 1,
        backup_sha256: row.get(13)?,
        acknowledges: row.get(14)?,
        state: ActivationState::parse(&state)?,
        requested_at: Timestamp::from_millis(row.get(16)?),
        resolved_at: row.get::<_, Option<i64>>(17)?.map(Timestamp::from_millis),
        witness_boot_id: row.get(18)?,
        witness_sequence: row.get::<_, Option<i64>>(19)?.map(|n| n as u64),
        detail: row.get(20)?,
    })
}

fn load_activation(conn: &Connection, id: i64) -> Result<Option<Activation>> {
    conn.query_row(
        &format!("SELECT {ACTIVATION_COLUMNS} FROM generation_activations WHERE id = ?1"),
        params![id],
        map_activation,
    )
    .optional()
    .map_err(StorageError::from)
}

/// What the caller asks for when it starts an activation or a rollback.
#[derive(Debug, Clone)]
pub struct ActivationRequest {
    pub generation: GenerationId,
    pub kind: ActivationKind,
    pub reason: String,
    /// Idempotency key (same key and request returns the same record).
    pub operation_id: Option<String>,
    pub expected_slot: String,
    pub expected_root_partuuid: String,
    /// Witness ledger position at request time (0 and an all-zero head for an
    /// empty ledger): only a strictly newer record can confirm this request.
    pub ledger_sequence: u64,
    pub ledger_head: String,
    pub irreversible: bool,
    pub backup_sha256: Option<String>,
    /// For re-activating a candidate that fell back: the FELL_BACK record the
    /// Admin has read and decided to retry despite.
    pub acknowledges: Option<i64>,
}

impl ActivationRequest {
    /// A request for an empty ledger.
    #[must_use]
    pub fn empty_ledger_head() -> String {
        ZERO_HEAD.to_owned()
    }

    fn validate(&self) -> Result<()> {
        validate_reason(&self.reason)?;
        if let Some(key) = &self.operation_id {
            validate_operation_id(key)?;
        }
        if !matches!(self.expected_slot.as_str(), "root-a" | "root-b") {
            return Err(invalid("expected_slot must be root-a or root-b"));
        }
        let uuid = &self.expected_root_partuuid;
        if uuid.len() != 36
            || !uuid.bytes().enumerate().all(|(i, b)| {
                if matches!(i, 8 | 13 | 18 | 23) {
                    b == b'-'
                } else {
                    matches!(b, b'0'..=b'9' | b'a'..=b'f')
                }
            })
        {
            return Err(invalid("expected_root_partuuid must be a lowercase UUID"));
        }
        if !is_hex(&self.ledger_head, 64) {
            return Err(invalid("ledger_head must be 64 lowercase hex digits"));
        }
        if i64::try_from(self.ledger_sequence).is_err() {
            return Err(invalid("ledger_sequence is out of range"));
        }
        match (&self.backup_sha256, self.irreversible) {
            (Some(hash), _) if !is_hex(hash, 64) => {
                return Err(invalid("backup_sha256 must be 64 lowercase hex digits"))
            }
            (None, true) => {
                return Err(invalid("an irreversible activation needs its backup hash"))
            }
            _ => {}
        }
        Ok(())
    }

    /// Digest of everything but the free-text reason, for idempotent retries.
    fn digest(&self, actor_uid: u32) -> String {
        let canonical = json!([
            "generation-activation-request/v1",
            self.generation.as_str(),
            self.kind.as_str(),
            actor_uid,
            self.expected_slot,
            self.expected_root_partuuid,
            self.ledger_sequence,
            self.ledger_head,
            self.irreversible,
            self.backup_sha256,
            self.acknowledges,
        ]);
        Sha256::digest(canonical.to_string().as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

/// What resolving the pending activation against the ledger did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    NoPending,
    /// Not decidable yet (no boot since the request, or no health record).
    Waiting {
        activation: i64,
        reason: String,
    },
    /// The ledger proves nothing about the request; it stays PENDING.
    Unresolved {
        activation: i64,
        reason: String,
    },
    Resolved(Box<Activation>),
}

/// Test-only process death at a named point of resolution (R2).
#[cfg(test)]
fn crash_point(name: &str) {
    if std::env::var("FLOWERLY_TEST_CRASH_AT").as_deref() == Ok(name) {
        std::process::abort();
    }
}

#[cfg(not(test))]
fn crash_point(_: &str) {}

/// The witnessed boot that resolves an activation.
#[derive(Debug, Clone)]
pub struct Witness {
    pub boot_id: String,
    pub sequence: u64,
}

/// Turn a database guard (trigger or CHECK) refusal into a typed conflict
/// carrying its message; other errors pass through.
fn guard<T>(result: rusqlite::Result<T>) -> Result<T> {
    match result {
        Err(rusqlite::Error::SqliteFailure(error, message))
            if error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER =>
        {
            Err(StorageError::OperationConflict(message.unwrap_or_else(
                || "refused by a database guard".to_owned(),
            )))
        }
        other => other.map_err(StorageError::from),
    }
}

fn require_generation(conn: &Connection, id: &GenerationId) -> Result<Generation> {
    load_generation(conn, id)?.ok_or_else(|| StorageError::NotFound(format!("generation {id}")))
}

fn audit_owner(generation: &Generation) -> Owner {
    generation.owner
}

/// Insert a PENDING activation row: every guard of migration 0015 runs.
fn insert_pending(
    tx: &Connection,
    admin: Caller,
    request: &ActivationRequest,
    digest: &str,
) -> Result<(Generation, i64)> {
    let generation = require_generation(tx, &request.generation)?;
    let pending: Option<i64> = tx
        .query_row(
            "SELECT id FROM generation_activations WHERE state = 'PENDING'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = pending {
        return Err(StorageError::OperationConflict(format!(
            "activation {id} is still PENDING"
        )));
    }
    let previous: Option<String> = tx
        .query_row(
            "SELECT id FROM generations WHERE status = 'STABLE'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    guard(tx.execute(
        "INSERT INTO generation_activations
             (generation, previous_generation, kind, actor_uid, reason, operation_id,
              request_digest, expected_slot, expected_root_partuuid, ledger_sequence,
              ledger_head, irreversible, backup_sha256, acknowledges, requested_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            request.generation.as_str(),
            previous,
            request.kind.as_str(),
            admin.uid(),
            request.reason,
            request.operation_id,
            digest,
            request.expected_slot,
            request.expected_root_partuuid,
            request.ledger_sequence as i64,
            request.ledger_head,
            i64::from(request.irreversible),
            request.backup_sha256,
            request.acknowledges,
            Timestamp::now().as_millis(),
        ],
    ))?;
    Ok((generation, tx.last_insert_rowid()))
}

impl Store {
    /// The current STABLE system Generation, if any.
    pub fn current_stable(&self) -> Result<Option<GenerationId>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM generations WHERE status = 'STABLE'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        raw.map(|id| {
            id.parse()
                .map_err(|_| StorageError::InvalidOperation("invalid stored generation id".into()))
        })
        .transpose()
    }

    /// Set the complete root identity and source commit of a CANDIDATE.
    /// Audited; refused once the Generation has left CANDIDATE.
    pub fn set_root_identity(
        &mut self,
        admin: Caller,
        id: &GenerationId,
        source_commit: &str,
        identity: &RootIdentity,
        reason: &str,
    ) -> Result<()> {
        validate_reason(reason)?;
        identity.validate()?;
        if !is_hex(source_commit, 40) {
            return Err(invalid("source_commit must be 40 lowercase hex digits"));
        }
        retry_on_busy(RetryConfig::default(), || {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let generation = require_generation(&tx, id)?;
            if generation.status != GenerationStatus::Candidate {
                return Err(StorageError::OperationConflict(format!(
                    "generation {id} is {}; its identity is frozen",
                    generation.status
                )));
            }
            guard(tx.execute(
                "UPDATE generations SET source_commit = ?2 WHERE id = ?1",
                params![id.as_str(), source_commit],
            ))?;
            guard(tx.execute(
                "INSERT INTO generation_identities
                     (generation, rootfs_sha256, kernel_sha256, initramfs_sha256,
                      input_manifest_sha256, build_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(generation) DO UPDATE SET
                     rootfs_sha256 = excluded.rootfs_sha256,
                     kernel_sha256 = excluded.kernel_sha256,
                     initramfs_sha256 = excluded.initramfs_sha256,
                     input_manifest_sha256 = excluded.input_manifest_sha256,
                     build_id = excluded.build_id",
                params![
                    id.as_str(),
                    identity.rootfs_sha256,
                    identity.kernel_sha256,
                    identity.initramfs_sha256,
                    identity.input_manifest_sha256,
                    identity.build_id,
                ],
            ))?;
            record_audit(
                &tx,
                admin,
                "generation.identity.set",
                ObjectRef::from(id).as_str(),
                audit_owner(&generation),
                audit_owner(&generation),
                reason,
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn root_identity(&self, id: &GenerationId) -> Result<Option<RootIdentity>> {
        self.conn
            .query_row(
                "SELECT rootfs_sha256, kernel_sha256, initramfs_sha256, input_manifest_sha256,
                        build_id
                 FROM generation_identities
                 WHERE generation = ?1 AND rootfs_sha256 IS NOT NULL
                   AND kernel_sha256 IS NOT NULL AND initramfs_sha256 IS NOT NULL
                   AND input_manifest_sha256 IS NOT NULL AND build_id IS NOT NULL",
                params![id.as_str()],
                |row| {
                    Ok(RootIdentity {
                        rootfs_sha256: row.get(0)?,
                        kernel_sha256: row.get(1)?,
                        initramfs_sha256: row.get(2)?,
                        input_manifest_sha256: row.get(3)?,
                        build_id: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// Record the outcome of an acceptance run of a CANDIDATE as an Experiment
    /// bound to it, naming the SHA-256 of the evidence archive. Recording the
    /// same evidence again returns the same Experiment. Audited.
    pub fn record_evidence(
        &mut self,
        admin: Caller,
        id: &GenerationId,
        outcome: EvidenceOutcome,
        evidence_sha256: &str,
        reason: &str,
    ) -> Result<ExperimentId> {
        validate_reason(reason)?;
        if !is_hex(evidence_sha256, 64) {
            return Err(invalid("evidence_sha256 must be 64 lowercase hex digits"));
        }
        retry_on_busy(RetryConfig::default(), || {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let generation = require_generation(&tx, id)?;
            if generation.status != GenerationStatus::Candidate {
                return Err(StorageError::OperationConflict(format!(
                    "generation {id} is {} (CANDIDATE required)",
                    generation.status
                )));
            }
            let existing: Option<(String, Option<String>)> = tx
                .query_row(
                    "SELECT id, result FROM experiments
                     WHERE candidate = ?1 AND json_extract(metrics, '$.evidence_sha256') = ?2",
                    params![id.as_str(), evidence_sha256],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((experiment, result)) = existing {
                if result.as_deref() != Some(outcome.as_str()) {
                    return Err(StorageError::OperationConflict(format!(
                        "evidence {evidence_sha256} was already recorded with another outcome"
                    )));
                }
                let experiment = experiment
                    .parse()
                    .map_err(|_| invalid("invalid stored experiment id"))?;
                tx.commit()?;
                return Ok(experiment);
            }
            let experiment = ExperimentId::new();
            tx.execute(
                "INSERT INTO experiments (id, hypothesis, candidate, metrics, result, created_at)
                 VALUES (?1, 'generation acceptance', ?2, ?3, ?4, ?5)",
                params![
                    experiment.as_str(),
                    id.as_str(),
                    json!({ "evidence_sha256": evidence_sha256 }).to_string(),
                    outcome.as_str(),
                    Timestamp::now().as_millis(),
                ],
            )?;
            record_audit(
                &tx,
                admin,
                "generation.evidence",
                ObjectRef::from(id).as_str(),
                audit_owner(&generation),
                audit_owner(&generation),
                reason,
            )?;
            tx.commit()?;
            Ok(experiment)
        })
    }

    /// `CANDIDATE` -> `TESTED`, Admin only (Q1), with verifiable evidence: an
    /// Experiment of this Generation with a passing result. The complete root identity is
    /// required by the database. Audited; adds a `TestedBy` provenance edge.
    pub fn declare_tested(
        &mut self,
        admin: Caller,
        id: &GenerationId,
        evidence: &ObjectRef,
        reason: &str,
    ) -> Result<Generation> {
        validate_reason(reason)?;
        let ObjectRef::Experiment(_) = evidence else {
            return Err(invalid(
                "evidence must be an Experiment with a recorded result",
            ));
        };
        retry_on_busy(RetryConfig::default(), || {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let found: Option<(Option<String>, Option<String>)> = tx
                .query_row(
                    "SELECT candidate, result FROM experiments WHERE id = ?1",
                    params![evidence.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((candidate, result)) = found else {
                return Err(StorageError::NotFound(format!(
                    "evidence {}",
                    evidence.as_str()
                )));
            };
            if candidate.as_deref() != Some(id.as_str()) {
                return Err(StorageError::OperationConflict(format!(
                    "evidence {} is not about generation {id}",
                    evidence.as_str()
                )));
            }
            if result.as_deref() != Some(EvidenceOutcome::Passed.as_str()) {
                return Err(StorageError::OperationConflict(format!(
                    "evidence {} has no passing result",
                    evidence.as_str()
                )));
            }
            let generation = require_generation(&tx, id)?;
            if generation.status != GenerationStatus::Candidate {
                return Err(StorageError::OperationConflict(format!(
                    "generation {id} is {} (CANDIDATE required)",
                    generation.status
                )));
            }
            guard(tx.execute(
                "UPDATE generations SET status = 'TESTED' WHERE id = ?1",
                params![id.as_str()],
            ))?;
            insert_provenance_row(
                &tx,
                &ProvenanceEdge {
                    from: ObjectRef::from(id),
                    relation: ProvenanceRelation::TestedBy,
                    to: evidence.clone(),
                    created_at: Timestamp::now(),
                    note: Some("declared TESTED by an Admin".to_owned()),
                },
            )?;
            record_audit(
                &tx,
                admin,
                "generation.test",
                ObjectRef::from(id).as_str(),
                audit_owner(&generation),
                audit_owner(&generation),
                reason,
            )?;
            let tested = require_generation(&tx, id)?;
            tx.commit()?;
            Ok(tested)
        })
    }

    /// `CANDIDATE`/`TESTED` -> `REJECTED` (terminal). Audited.
    pub fn reject_generation(
        &mut self,
        admin: Caller,
        id: &GenerationId,
        reason: &str,
    ) -> Result<Generation> {
        self.finish_generation(
            admin,
            id,
            reason,
            GenerationStatus::Rejected,
            "generation.reject",
        )
    }

    /// `TESTED`/`SUPERSEDED` -> `REVOKED` (terminal). The active `STABLE`
    /// Generation cannot be revoked until another is confirmed (Q5). Audited.
    pub fn revoke_generation(
        &mut self,
        admin: Caller,
        id: &GenerationId,
        reason: &str,
    ) -> Result<Generation> {
        self.finish_generation(
            admin,
            id,
            reason,
            GenerationStatus::Revoked,
            "generation.revoke",
        )
    }

    fn finish_generation(
        &mut self,
        admin: Caller,
        id: &GenerationId,
        reason: &str,
        to: GenerationStatus,
        command: &str,
    ) -> Result<Generation> {
        validate_reason(reason)?;
        retry_on_busy(RetryConfig::default(), || {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let generation = require_generation(&tx, id)?;
            if generation.status == to {
                return Err(StorageError::OperationConflict(format!(
                    "generation {id} is already {to}"
                )));
            }
            if to == GenerationStatus::Revoked && generation.status == GenerationStatus::Stable {
                return Err(StorageError::OperationConflict(format!(
                    "generation {id} is the active STABLE Generation; confirm another one first"
                )));
            }
            guard(tx.execute(
                "UPDATE generations SET status = ?2 WHERE id = ?1",
                params![id.as_str(), to.to_string()],
            ))?;
            record_audit(
                &tx,
                admin,
                command,
                ObjectRef::from(id).as_str(),
                audit_owner(&generation),
                audit_owner(&generation),
                reason,
            )?;
            let updated = require_generation(&tx, id)?;
            tx.commit()?;
            Ok(updated)
        })
    }

    /// Record a durable `PENDING` activation or rollback request. It changes no
    /// Generation status. A retry with the same `operation_id` and request
    /// returns the same record; the same key with a different request is a
    /// conflict. Audited.
    pub fn request_activation(
        &mut self,
        admin: Caller,
        request: &ActivationRequest,
    ) -> Result<Activation> {
        request.validate()?;
        let digest = request.digest(admin.uid());
        retry_on_busy(RetryConfig::default(), || {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            if let Some(key) = &request.operation_id {
                let existing: Option<(i64, String)> = tx
                    .query_row(
                        "SELECT id, request_digest FROM generation_activations
                         WHERE operation_id = ?1",
                        params![key],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                if let Some((id, stored)) = existing {
                    if stored != digest {
                        return Err(StorageError::OperationConflict(
                            "operation_id was already used for a different activation request"
                                .into(),
                        ));
                    }
                    let found = load_activation(&tx, id)?
                        .ok_or_else(|| StorageError::NotFound(format!("activation {id}")))?;
                    tx.commit()?;
                    return Ok(found);
                }
            }
            let (generation, id) = insert_pending(&tx, admin, request, &digest)?;
            record_audit(
                &tx,
                admin,
                match request.kind {
                    ActivationKind::Activate => "generation.activate.request",
                    ActivationKind::Rollback => "generation.rollback.request",
                },
                ObjectRef::from(&request.generation).as_str(),
                audit_owner(&generation),
                audit_owner(&generation),
                &request.reason,
            )?;
            let created = load_activation(&tx, id)?
                .ok_or_else(|| StorageError::NotFound(format!("activation {id}")))?;
            tx.commit()?;
            Ok(created)
        })
    }

    /// Run every check of [`Store::request_activation`] (validation, the one
    /// PENDING rule and the database guards) in a transaction that is rolled
    /// back: nothing is written or audited. For checking before a backup is
    /// taken or the broker is asked.
    pub fn check_activation_request(
        &mut self,
        admin: Caller,
        request: &ActivationRequest,
    ) -> Result<()> {
        request.validate()?;
        let digest = request.digest(admin.uid());
        retry_on_busy(RetryConfig::default(), || {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            insert_pending(&tx, admin, request, &digest)?;
            tx.rollback()?;
            Ok(())
        })
    }

    /// The activation recorded under an idempotency key, if any.
    pub fn activation_for_operation(&self, operation_id: &str) -> Result<Option<Activation>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {ACTIVATION_COLUMNS} FROM generation_activations WHERE operation_id = ?1"
                ),
                params![operation_id],
                map_activation,
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// `PENDING` -> `ABANDONED` (the slot could not be armed). Idempotent.
    pub fn abandon_activation(&mut self, id: i64, detail: &str) -> Result<Activation> {
        self.resolve_activation(id, ActivationState::Abandoned, None, detail)
    }

    /// `PENDING` -> `FELL_BACK`: the witness shows the previous root came back.
    /// The Generation keeps its status. Idempotent.
    pub fn record_fell_back(
        &mut self,
        id: i64,
        witness: Option<&Witness>,
        detail: &str,
    ) -> Result<Activation> {
        self.resolve_activation(id, ActivationState::FellBack, witness, detail)
    }

    /// `PENDING` -> `CONFIRMED`, and in the same transaction the previous
    /// `STABLE` becomes `SUPERSEDED` and the activated Generation `STABLE`.
    /// The caller (witness resolution, G3) has verified the boot; the database
    /// still refuses a witness that is not strictly newer than the request.
    /// Idempotent for an already confirmed record.
    pub fn confirm_activation(
        &mut self,
        id: i64,
        witness: &Witness,
        detail: &str,
    ) -> Result<Activation> {
        self.resolve_activation(id, ActivationState::Confirmed, Some(witness), detail)
    }

    fn resolve_activation(
        &mut self,
        id: i64,
        to: ActivationState,
        witness: Option<&Witness>,
        detail: &str,
    ) -> Result<Activation> {
        if detail.len() > MAX_DETAIL {
            return Err(invalid("detail is too long"));
        }
        if let Some(w) = witness {
            if w.boot_id.is_empty() || w.boot_id.len() > 64 || i64::try_from(w.sequence).is_err() {
                return Err(invalid("invalid witness"));
            }
        }
        if to == ActivationState::Confirmed && witness.is_none() {
            return Err(invalid("a confirmation needs the witnessed boot"));
        }
        retry_on_busy(RetryConfig::default(), || {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            crash_point("resolve:begin");
            let current = load_activation(&tx, id)?
                .ok_or_else(|| StorageError::NotFound(format!("activation {id}")))?;
            if current.state == to {
                // A retry of the same resolution, not a second opinion on it.
                if let (ActivationState::Confirmed, Some(w)) = (to, witness) {
                    if current.witness_boot_id.as_deref() != Some(w.boot_id.as_str())
                        || current.witness_sequence != Some(w.sequence)
                    {
                        return Err(StorageError::OperationConflict(format!(
                            "activation {id} was confirmed by another witness"
                        )));
                    }
                }
                tx.commit()?;
                return Ok(current);
            }
            if current.state != ActivationState::Pending {
                return Err(StorageError::OperationConflict(format!(
                    "activation {id} is already {}",
                    current.state.as_str()
                )));
            }
            if let (ActivationState::Confirmed, Some(w)) = (to, witness) {
                if w.sequence <= current.ledger_sequence {
                    return Err(StorageError::OperationConflict(format!(
                        "witness sequence {} is not newer than the request ({})",
                        w.sequence, current.ledger_sequence
                    )));
                }
            }
            guard(tx.execute(
                "UPDATE generation_activations
                 SET state = ?2, resolved_at = ?3, witness_boot_id = ?4,
                     witness_sequence = ?5, detail = ?6
                 WHERE id = ?1",
                params![
                    id,
                    to.as_str(),
                    Timestamp::now().as_millis(),
                    witness.map(|w| w.boot_id.clone()),
                    witness.map(|w| w.sequence as i64),
                    if detail.is_empty() {
                        None
                    } else {
                        Some(detail)
                    },
                ],
            ))?;
            crash_point("resolve:activation");
            if to == ActivationState::Confirmed {
                if let Some(previous) = &current.previous_generation {
                    guard(tx.execute(
                        "UPDATE generations SET status = 'SUPERSEDED'
                         WHERE id = ?1 AND status = 'STABLE'",
                        params![previous.as_str()],
                    ))?;
                }
                crash_point("resolve:superseded");
                guard(tx.execute(
                    "UPDATE generations SET status = 'STABLE' WHERE id = ?1",
                    params![current.generation.as_str()],
                ))?;
                crash_point("resolve:stable");
            }
            let resolved = load_activation(&tx, id)?
                .ok_or_else(|| StorageError::NotFound(format!("activation {id}")))?;
            tx.commit()?;
            crash_point("resolve:committed");
            Ok(resolved)
        })
    }

    /// Resolve the pending activation, if any, from the verified ledger and
    /// this kernel's boot id (ADR-0023 R1, R2). Waiting and unresolved leave it
    /// PENDING; each outcome is written in one IMMEDIATE transaction and a
    /// repeated call after it is a no-op.
    pub fn resolve_pending_activation(
        &mut self,
        ledger: &Ledger,
        live_boot_id: &str,
    ) -> Result<Resolution> {
        let Some(pending) = self.pending_activation()? else {
            return Ok(Resolution::NoPending);
        };
        let generation = require_generation(&self.conn, &pending.generation)?;
        let (Some(source_commit), Some(identity)) = (
            generation.source_commit,
            self.root_identity(&pending.generation)?,
        ) else {
            return Ok(Resolution::Unresolved {
                activation: pending.id,
                reason: "the Generation has no complete root identity".into(),
            });
        };
        let expectation = Expectation {
            slot: pending.expected_slot.clone(),
            root_partuuid: pending.expected_root_partuuid.clone(),
            source_commit,
            input_manifest_sha256: identity.input_manifest_sha256,
            build_id: identity.build_id,
            ledger_sequence: pending.ledger_sequence,
            ledger_head: pending.ledger_head.clone(),
        };
        Ok(match decide(ledger, &expectation, live_boot_id) {
            Decision::Wait(reason) => Resolution::Waiting {
                activation: pending.id,
                reason,
            },
            Decision::Unresolved(reason) => Resolution::Unresolved {
                activation: pending.id,
                reason,
            },
            Decision::Confirm {
                boot_id,
                sequence,
                detail,
            } => Resolution::Resolved(Box::new(self.confirm_activation(
                pending.id,
                &Witness { boot_id, sequence },
                &detail,
            )?)),
            Decision::FellBack {
                boot_id,
                sequence,
                detail,
            } => Resolution::Resolved(Box::new(self.record_fell_back(
                pending.id,
                Some(&Witness { boot_id, sequence }),
                &detail,
            )?)),
            Decision::Abandon(detail) => {
                Resolution::Resolved(Box::new(self.abandon_activation(pending.id, &detail)?))
            }
        })
    }

    /// The one unresolved activation, if any.
    pub fn pending_activation(&self) -> Result<Option<Activation>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {ACTIVATION_COLUMNS} FROM generation_activations WHERE state = 'PENDING'"
                ),
                [],
                map_activation,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn activation(&self, id: i64) -> Result<Option<Activation>> {
        load_activation(&self.conn, id)
    }

    /// The whole activation history, oldest first.
    pub fn activation_history(&self) -> Result<Vec<Activation>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ACTIVATION_COLUMNS} FROM generation_activations ORDER BY id"
        ))?;
        let rows = stmt.query_map([], map_activation)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MIGRATIONS;
    use crate::types::ExperimentId;

    const ADMIN: Caller = Caller::new(0);
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const PARTUUID: &str = "11111111-2222-3333-4444-555555555555";
    const LEDGER_SEQUENCE: u64 = 5;
    const STATES: [&str; 6] = [
        "CANDIDATE",
        "TESTED",
        "STABLE",
        "SUPERSEDED",
        "REJECTED",
        "REVOKED",
    ];
    const LEGAL: [(&str, &str); 8] = [
        ("CANDIDATE", "TESTED"),
        ("CANDIDATE", "REJECTED"),
        ("TESTED", "REJECTED"),
        ("TESTED", "STABLE"),
        ("STABLE", "SUPERSEDED"),
        ("SUPERSEDED", "STABLE"),
        ("TESTED", "REVOKED"),
        ("SUPERSEDED", "REVOKED"),
    ];

    fn store() -> Store {
        let mut store = Store::open_in_memory().expect("open store");
        store.migrate().expect("migrate");
        store
    }

    fn hash(c: char) -> String {
        c.to_string().repeat(64)
    }

    fn identity() -> RootIdentity {
        RootIdentity {
            rootfs_sha256: hash('a'),
            kernel_sha256: hash('b'),
            initramfs_sha256: hash('c'),
            input_manifest_sha256: hash('d'),
            build_id: "build-1".to_owned(),
        }
    }

    fn candidate_owned(store: &Store, owner: Owner) -> GenerationId {
        let generation = Generation {
            id: GenerationId::new(),
            parent: None,
            source_commit: None,
            system_manifest: None,
            reason_goal: None,
            experiment: None,
            status: GenerationStatus::Candidate,
            created_at: Timestamp::now(),
            owner,
        };
        store.insert_generation(&generation).expect("insert");
        generation.id
    }

    fn candidate(store: &Store) -> GenerationId {
        candidate_owned(store, Owner::System)
    }

    fn evidence_for(store: &Store, id: &GenerationId) -> ObjectRef {
        let experiment = ExperimentId::new();
        store
            .conn
            .execute(
                "INSERT INTO experiments (id, hypothesis, candidate, result, created_at)
                 VALUES (?1, 'it boots', ?2, 'passed', 1)",
                params![experiment.as_str(), id.as_str()],
            )
            .unwrap();
        ObjectRef::from(&experiment)
    }

    fn make_tested(store: &mut Store, id: &GenerationId) {
        store
            .set_root_identity(ADMIN, id, COMMIT, &identity(), "built")
            .unwrap();
        let evidence = evidence_for(store, id);
        store
            .declare_tested(ADMIN, id, &evidence, "passed")
            .unwrap();
    }

    fn tested(store: &mut Store) -> GenerationId {
        let id = candidate(store);
        make_tested(store, &id);
        id
    }

    fn request(generation: &GenerationId, kind: ActivationKind) -> ActivationRequest {
        ActivationRequest {
            generation: generation.clone(),
            kind,
            reason: "go".to_owned(),
            operation_id: None,
            expected_slot: "root-b".to_owned(),
            expected_root_partuuid: PARTUUID.to_owned(),
            ledger_sequence: LEDGER_SEQUENCE,
            ledger_head: hash('e'),
            irreversible: false,
            backup_sha256: None,
            acknowledges: None,
        }
    }

    fn witness(sequence: u64) -> Witness {
        Witness {
            boot_id: "boot-1".to_owned(),
            sequence,
        }
    }

    fn activate(store: &mut Store, id: &GenerationId, kind: ActivationKind) -> Activation {
        let pending = store.request_activation(ADMIN, &request(id, kind)).unwrap();
        store
            .confirm_activation(pending.id, &witness(LEDGER_SEQUENCE + 1), "ok")
            .unwrap()
    }

    fn stable(store: &mut Store) -> GenerationId {
        let id = tested(store);
        activate(store, &id, ActivationKind::Activate);
        id
    }

    /// Returns (superseded, current stable).
    fn superseded(store: &mut Store) -> (GenerationId, GenerationId) {
        let old = stable(store);
        let new = stable(store);
        (old, new)
    }

    fn in_status(store: &mut Store, status: &str) -> GenerationId {
        match status {
            "CANDIDATE" => candidate(store),
            "TESTED" => tested(store),
            "STABLE" => stable(store),
            "SUPERSEDED" => superseded(store).0,
            "REJECTED" => {
                let id = candidate(store);
                store.reject_generation(ADMIN, &id, "bad").unwrap();
                id
            }
            "REVOKED" => {
                let id = tested(store);
                store.revoke_generation(ADMIN, &id, "bad").unwrap();
                id
            }
            other => panic!("unknown status {other}"),
        }
    }

    fn status(store: &Store, id: &GenerationId) -> GenerationStatus {
        store.get_generation(id).unwrap().unwrap().status
    }

    fn raw_status(store: &Store, id: &GenerationId, to: &str) -> rusqlite::Result<usize> {
        store.conn.execute(
            "UPDATE generations SET status = ?2 WHERE id = ?1",
            params![id.as_str(), to],
        )
    }

    fn assert_conflict<T: std::fmt::Debug>(result: Result<T>) {
        assert!(
            matches!(result, Err(StorageError::OperationConflict(_))),
            "{result:?}"
        );
    }

    fn assert_invalid<T: std::fmt::Debug>(result: Result<T>) {
        assert!(
            matches!(result, Err(StorageError::InvalidOperation(_))),
            "{result:?}"
        );
    }

    fn assert_refused(result: rusqlite::Result<usize>, needle: &str) {
        let error = result.expect_err(needle).to_string();
        assert!(error.contains(needle), "{error} should contain {needle}");
    }

    // --- transitions -----------------------------------------------------

    #[test]
    fn transition_table_is_exactly_the_adr_table() {
        let store = store();
        let mut stmt = store
            .conn
            .prepare("SELECT from_status, to_status FROM generation_transitions")
            .unwrap();
        let mut found: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        found.sort();
        let mut expected: Vec<(String, String)> = LEGAL
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect();
        expected.sort();
        assert_eq!(found, expected);
    }

    #[test]
    fn transition_table_cannot_be_changed() {
        let store = store();
        for sql in [
            "INSERT INTO generation_transitions VALUES ('REVOKED', 'STABLE')",
            "UPDATE generation_transitions SET to_status = 'STABLE'
             WHERE from_status = 'CANDIDATE'",
            "DELETE FROM generation_transitions",
        ] {
            assert_refused(store.conn.execute(sql, []), "transition table is fixed");
        }
    }

    #[test]
    fn every_illegal_pair_is_refused_by_the_database() {
        for from in STATES {
            for to in STATES {
                if from == to || LEGAL.contains(&(from, to)) {
                    continue;
                }
                let mut store = store();
                let id = in_status(&mut store, from);
                assert!(
                    raw_status(&store, &id, to).is_err(),
                    "{from} -> {to} must be refused"
                );
                assert_eq!(status(&store, &id).to_string(), from, "{from} -> {to}");
            }
        }
    }

    #[test]
    fn unknown_status_is_refused() {
        let mut store = store();
        let id = tested(&mut store);
        assert_refused(
            raw_status(&store, &id, "ACTIVE"),
            "illegal Generation status transition",
        );
    }

    #[test]
    fn terminal_transitions_without_extra_conditions_are_legal() {
        for (from, to) in [
            ("CANDIDATE", "REJECTED"),
            ("TESTED", "REJECTED"),
            ("TESTED", "REVOKED"),
            ("SUPERSEDED", "REVOKED"),
        ] {
            let mut store = store();
            let id = in_status(&mut store, from);
            raw_status(&store, &id, to).unwrap();
            assert_eq!(status(&store, &id).to_string(), to);
        }
    }

    #[test]
    fn stable_cannot_be_revoked_even_directly() {
        let mut store = store();
        let id = stable(&mut store);
        assert_refused(
            raw_status(&store, &id, "REVOKED"),
            "illegal Generation status transition",
        );
        assert_conflict(store.revoke_generation(ADMIN, &id, "no"));
        assert_eq!(status(&store, &id), GenerationStatus::Stable);
    }

    #[test]
    fn a_new_generation_must_start_as_candidate() {
        let store = store();
        for bad in ["TESTED", "STABLE", "SUPERSEDED", "REJECTED", "REVOKED"] {
            assert_refused(
                store.conn.execute(
                    "INSERT INTO generations (id, status, created_at, owner_class)
                     VALUES ('gen-x', ?1, 1, 'system')",
                    params![bad],
                ),
                "starts as CANDIDATE",
            );
        }
    }

    #[test]
    fn a_generation_row_cannot_be_deleted_replaced_or_renamed() {
        let mut store = store();
        let id = stable(&mut store);
        assert_refused(
            store.conn.execute(
                "DELETE FROM generations WHERE id = ?1",
                params![id.as_str()],
            ),
            "cannot be deleted",
        );
        assert_refused(
            store.conn.execute(
                "INSERT OR REPLACE INTO generations (id, status, created_at, owner_class)
                 VALUES (?1, 'CANDIDATE', 1, 'system')",
                params![id.as_str()],
            ),
            "already exists",
        );
        assert_refused(
            store.conn.execute(
                "UPDATE generations SET id = 'gen-other' WHERE id = ?1",
                params![id.as_str()],
            ),
            "id is immutable",
        );
        assert_eq!(status(&store, &id), GenerationStatus::Stable);
    }

    // --- STABLE only through a confirmed activation --------------------------

    #[test]
    fn stable_cannot_be_set_directly() {
        let mut store = store();
        let id = tested(&mut store);
        assert_refused(
            raw_status(&store, &id, "STABLE"),
            "only through a confirmed activation",
        );
        // A pending request is not a confirmation.
        store
            .request_activation(ADMIN, &request(&id, ActivationKind::Activate))
            .unwrap();
        assert_refused(
            raw_status(&store, &id, "STABLE"),
            "only through a confirmed activation",
        );
    }

    #[test]
    fn an_abandoned_or_foreign_confirmation_does_not_allow_stable() {
        let mut store = store();
        let id = tested(&mut store);
        let pending = store
            .request_activation(ADMIN, &request(&id, ActivationKind::Activate))
            .unwrap();
        store.abandon_activation(pending.id, "no slot").unwrap();
        assert_refused(
            raw_status(&store, &id, "STABLE"),
            "only through a confirmed activation",
        );
        stable(&mut store);
        assert_refused(
            raw_status(&store, &id, "STABLE"),
            "only through a confirmed activation",
        );
    }

    #[test]
    fn superseded_cannot_return_to_stable_without_a_rollback_confirmation() {
        let mut store = store();
        let (old, _new) = superseded(&mut store);
        assert_refused(
            raw_status(&store, &old, "STABLE"),
            "only through a confirmed activation",
        );
    }

    #[test]
    fn stable_cannot_be_superseded_directly() {
        let mut store = store();
        let id = stable(&mut store);
        assert_refused(
            raw_status(&store, &id, "SUPERSEDED"),
            "superseded only by a confirmed activation",
        );
    }

    #[test]
    fn the_index_allows_only_one_stable_even_with_a_forged_confirmation() {
        let mut store = store();
        let first = stable(&mut store);
        let second = tested(&mut store);
        let pending = store
            .request_activation(ADMIN, &request(&second, ActivationKind::Activate))
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE generation_activations
                 SET state = 'CONFIRMED', resolved_at = 1, witness_boot_id = 'b',
                     witness_sequence = 99
                 WHERE id = ?1",
                params![pending.id],
            )
            .unwrap();
        assert_refused(raw_status(&store, &second, "STABLE"), "UNIQUE");
        assert_eq!(status(&store, &first), GenerationStatus::Stable);
    }

    // --- identity ------------------------------------------------------------

    #[test]
    fn tested_needs_a_complete_identity() {
        let mut store = store();
        let id = candidate(&store);
        let evidence = evidence_for(&store, &id);
        assert_conflict(store.declare_tested(ADMIN, &id, &evidence, "no identity"));

        store
            .conn
            .execute(
                "UPDATE generations SET source_commit = ?2 WHERE id = ?1",
                params![id.as_str(), COMMIT],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO generation_identities
                     (generation, rootfs_sha256, kernel_sha256, initramfs_sha256)
                 VALUES (?1, ?2, ?2, ?2)",
                params![id.as_str(), hash('a')],
            )
            .unwrap();
        assert_conflict(store.declare_tested(ADMIN, &id, &evidence, "partial identity"));
        assert_eq!(status(&store, &id), GenerationStatus::Candidate);
    }

    #[test]
    fn tested_needs_a_full_source_commit() {
        let mut store = store();
        let id = candidate(&store);
        store
            .set_root_identity(ADMIN, &id, COMMIT, &identity(), "built")
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE generations SET source_commit = 'abc' WHERE id = ?1",
                params![id.as_str()],
            )
            .unwrap();
        let evidence = evidence_for(&store, &id);
        assert_conflict(store.declare_tested(ADMIN, &id, &evidence, "short commit"));
    }

    #[test]
    fn invalid_identities_are_refused_before_and_by_the_database() {
        let mut store = store();
        let id = candidate(&store);
        let mut upper = identity();
        upper.rootfs_sha256 = "A".repeat(64);
        let mut short = identity();
        short.kernel_sha256 = "a".repeat(63);
        let mut build = identity();
        build.build_id = "bad id/".to_owned();
        let mut empty = identity();
        empty.build_id = String::new();
        for bad in [upper, short, build, empty] {
            assert_invalid(store.set_root_identity(ADMIN, &id, COMMIT, &bad, "x"));
        }
        assert_invalid(store.set_root_identity(ADMIN, &id, "abc", &identity(), "x"));
        assert_invalid(store.set_root_identity(ADMIN, &id, &"A".repeat(40), &identity(), "x"));
        assert!(store.root_identity(&id).unwrap().is_none());

        assert!(store
            .conn
            .execute(
                "INSERT INTO generation_identities (generation, rootfs_sha256) VALUES (?1, 'xyz')",
                params![id.as_str()],
            )
            .is_err());
    }

    #[test]
    fn identity_can_change_on_a_candidate_and_is_frozen_afterwards() {
        let mut store = store();
        let id = candidate(&store);
        store
            .set_root_identity(ADMIN, &id, COMMIT, &identity(), "built")
            .unwrap();
        let mut newer = identity();
        newer.build_id = "build-2".to_owned();
        store
            .set_root_identity(ADMIN, &id, COMMIT, &newer, "rebuilt")
            .unwrap();
        assert_eq!(store.root_identity(&id).unwrap().unwrap(), newer);

        let evidence = evidence_for(&store, &id);
        store.declare_tested(ADMIN, &id, &evidence, "ok").unwrap();

        let refused = store
            .set_root_identity(ADMIN, &id, COMMIT, &identity(), "again")
            .unwrap_err()
            .to_string();
        assert!(refused.contains("its identity is frozen"), "{refused}");
        assert_refused(
            store.conn.execute(
                "UPDATE generation_identities SET build_id = 'x' WHERE generation = ?1",
                params![id.as_str()],
            ),
            "can only change while",
        );
        assert_refused(
            store.conn.execute(
                "DELETE FROM generation_identities WHERE generation = ?1",
                params![id.as_str()],
            ),
            "never deleted",
        );
        assert_refused(
            store.conn.execute(
                "UPDATE generations SET source_commit = ?2 WHERE id = ?1",
                params![id.as_str(), "f".repeat(40)],
            ),
            "frozen",
        );
        assert_refused(
            store.conn.execute(
                "UPDATE generations SET system_manifest = 'other' WHERE id = ?1",
                params![id.as_str()],
            ),
            "frozen",
        );
    }

    #[test]
    fn an_identity_cannot_be_added_after_candidate() {
        let mut store = store();
        let id = in_status(&mut store, "REJECTED");
        assert_refused(
            store.conn.execute(
                "INSERT INTO generation_identities (generation) VALUES (?1)",
                params![id.as_str()],
            ),
            "only be set while",
        );
    }

    // --- TESTED evidence and audit -------------------------------------------

    #[test]
    fn declaring_tested_records_evidence_and_audit() {
        let mut store = store();
        let id = candidate(&store);
        store
            .set_root_identity(ADMIN, &id, COMMIT, &identity(), "built")
            .unwrap();
        let evidence = evidence_for(&store, &id);
        let result = store
            .declare_tested(Caller::new(1234), &id, &evidence, "passed")
            .unwrap();
        assert_eq!(result.status, GenerationStatus::Tested);
        let edges = store.provenance_from(&ObjectRef::from(&id)).unwrap();
        assert!(edges
            .iter()
            .any(|e| e.relation == ProvenanceRelation::TestedBy && e.to == evidence));
        let audit = store.admin_audit_for_object(&ObjectRef::from(&id)).unwrap();
        let record = audit
            .iter()
            .find(|r| r.command == "generation.test")
            .expect("audited");
        assert_eq!((record.actor_uid, record.reason.as_str()), (1234, "passed"));
    }

    #[test]
    fn tested_needs_evidence_about_this_generation() {
        let mut store = store();
        let id = candidate(&store);
        store
            .set_root_identity(ADMIN, &id, COMMIT, &identity(), "built")
            .unwrap();
        let other = candidate(&store);

        let insert = |store: &Store, candidate: Option<&str>, result: Option<&str>| {
            let experiment = ExperimentId::new();
            store
                .conn
                .execute(
                    "INSERT INTO experiments (id, hypothesis, candidate, result, created_at)
                     VALUES (?1, 'h', ?2, ?3, 1)",
                    params![experiment.as_str(), candidate, result],
                )
                .unwrap();
            ObjectRef::from(&experiment)
        };
        for evidence in [
            insert(&store, Some(id.as_str()), None),
            insert(&store, Some(id.as_str()), Some("   ")),
            insert(&store, Some(other.as_str()), Some("passed")),
            insert(&store, None, Some("passed")),
            insert(&store, Some(id.as_str()), Some("failed")),
        ] {
            assert_conflict(store.declare_tested(ADMIN, &id, &evidence, "x"));
            assert_eq!(status(&store, &id), GenerationStatus::Candidate);
        }

        let missing = ObjectRef::from(&ExperimentId::new());
        assert!(matches!(
            store.declare_tested(ADMIN, &id, &missing, "x"),
            Err(StorageError::NotFound(_))
        ));
        assert_invalid(store.declare_tested(ADMIN, &id, &ObjectRef::from(&other), "x"));
        assert!(store
            .admin_audit_for_object(&ObjectRef::from(&id))
            .unwrap()
            .iter()
            .all(|r| r.command != "generation.test"));
    }

    #[test]
    fn only_a_candidate_can_be_declared_tested() {
        let mut store = store();
        let id = tested(&mut store);
        let evidence = evidence_for(&store, &id);
        assert_conflict(store.declare_tested(ADMIN, &id, &evidence, "again"));
    }

    #[test]
    fn a_blank_reason_is_refused_everywhere() {
        let mut store = store();
        let id = tested(&mut store);
        let evidence = evidence_for(&store, &id);
        assert_invalid(store.declare_tested(ADMIN, &id, &evidence, " "));
        assert_invalid(store.reject_generation(ADMIN, &id, ""));
        assert_invalid(store.revoke_generation(ADMIN, &id, "  "));
        assert_invalid(store.set_root_identity(ADMIN, &id, COMMIT, &identity(), " "));
    }

    // --- recorded evidence ---------------------------------------------------

    #[test]
    fn evidence_is_bound_to_the_generation_and_audited() {
        let mut store = store();
        let id = candidate(&store);
        store
            .set_root_identity(ADMIN, &id, COMMIT, &identity(), "built")
            .unwrap();
        let experiment = store
            .record_evidence(
                Caller::new(77),
                &id,
                EvidenceOutcome::Passed,
                &hash('9'),
                "kvm run",
            )
            .unwrap();
        let (candidate_ref, result, metrics): (String, String, String) = store
            .conn
            .query_row(
                "SELECT candidate, result, metrics FROM experiments WHERE id = ?1",
                params![experiment.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(candidate_ref, id.as_str());
        assert_eq!(result, "passed");
        let metrics: serde_json::Value = serde_json::from_str(&metrics).unwrap();
        assert_eq!(metrics["evidence_sha256"], hash('9'));
        let audit = store.admin_audit_for_object(&ObjectRef::from(&id)).unwrap();
        let record = audit
            .iter()
            .find(|r| r.command == "generation.evidence")
            .expect("audited");
        assert_eq!((record.actor_uid, record.reason.as_str()), (77, "kvm run"));

        let result = store
            .declare_tested(ADMIN, &id, &ObjectRef::from(&experiment), "passed")
            .unwrap();
        assert_eq!(result.status, GenerationStatus::Tested);
    }

    #[test]
    fn failed_evidence_cannot_back_tested() {
        let mut store = store();
        let id = candidate(&store);
        store
            .set_root_identity(ADMIN, &id, COMMIT, &identity(), "built")
            .unwrap();
        let experiment = store
            .record_evidence(ADMIN, &id, EvidenceOutcome::Failed, &hash('9'), "kvm run")
            .unwrap();
        assert_conflict(store.declare_tested(ADMIN, &id, &ObjectRef::from(&experiment), "no"));
        assert_eq!(status(&store, &id), GenerationStatus::Candidate);
    }

    #[test]
    fn recording_the_same_evidence_again_returns_the_same_experiment() {
        let mut store = store();
        let id = candidate(&store);
        let first = store
            .record_evidence(ADMIN, &id, EvidenceOutcome::Passed, &hash('9'), "run")
            .unwrap();
        let retry = store
            .record_evidence(ADMIN, &id, EvidenceOutcome::Passed, &hash('9'), "retry")
            .unwrap();
        assert_eq!(first, retry);
        let audit = store.admin_audit_for_object(&ObjectRef::from(&id)).unwrap();
        assert_eq!(
            audit
                .iter()
                .filter(|r| r.command == "generation.evidence")
                .count(),
            1
        );
    }

    #[test]
    fn the_same_evidence_cannot_change_outcome() {
        let mut store = store();
        let id = candidate(&store);
        store
            .record_evidence(ADMIN, &id, EvidenceOutcome::Failed, &hash('9'), "run")
            .unwrap();
        assert_conflict(store.record_evidence(
            ADMIN,
            &id,
            EvidenceOutcome::Passed,
            &hash('9'),
            "x",
        ));
    }

    #[test]
    fn evidence_is_validated_and_only_for_a_candidate() {
        let mut store = store();
        let id = candidate(&store);
        for bad in ["xyz".to_owned(), "A".repeat(64), "a".repeat(63)] {
            assert_invalid(store.record_evidence(ADMIN, &id, EvidenceOutcome::Passed, &bad, "x"));
        }
        assert_invalid(store.record_evidence(ADMIN, &id, EvidenceOutcome::Passed, &hash('9'), " "));
        let missing = GenerationId::new();
        assert!(matches!(
            store.record_evidence(ADMIN, &missing, EvidenceOutcome::Passed, &hash('9'), "x"),
            Err(StorageError::NotFound(_))
        ));
        let done = tested(&mut store);
        assert_conflict(store.record_evidence(
            ADMIN,
            &done,
            EvidenceOutcome::Passed,
            &hash('9'),
            "x",
        ));
    }

    #[test]
    fn evidence_outcome_parses_only_known_values() {
        assert_eq!(
            "passed".parse::<EvidenceOutcome>(),
            Ok(EvidenceOutcome::Passed)
        );
        assert_eq!(
            "failed".parse::<EvidenceOutcome>(),
            Ok(EvidenceOutcome::Failed)
        );
        for bad in ["", "PASSED", "ok"] {
            assert!(bad.parse::<EvidenceOutcome>().is_err());
        }
    }

    // --- reject and revoke ---------------------------------------------------

    #[test]
    fn reject_and_revoke_are_terminal_and_audited() {
        let mut store = store();
        let rejected = candidate(&store);
        let result = store.reject_generation(ADMIN, &rejected, "broken").unwrap();
        assert_eq!(result.status, GenerationStatus::Rejected);
        let revoked = tested(&mut store);
        let result = store.revoke_generation(ADMIN, &revoked, "cve").unwrap();
        assert_eq!(result.status, GenerationStatus::Revoked);

        for id in [&rejected, &revoked] {
            assert_conflict(store.reject_generation(ADMIN, id, "again"));
            assert_conflict(store.revoke_generation(ADMIN, id, "again"));
        }
        let audit = store
            .admin_audit_for_object(&ObjectRef::from(&revoked))
            .unwrap();
        assert!(audit
            .iter()
            .any(|r| r.command == "generation.revoke" && r.reason == "cve"));
        let audit = store
            .admin_audit_for_object(&ObjectRef::from(&rejected))
            .unwrap();
        assert!(audit
            .iter()
            .any(|r| r.command == "generation.reject" && r.reason == "broken"));
    }

    #[test]
    fn a_candidate_cannot_be_revoked() {
        let mut store = store();
        let id = candidate(&store);
        assert_conflict(store.revoke_generation(ADMIN, &id, "x"));
    }

    #[test]
    fn the_active_stable_is_revocable_only_after_another_is_confirmed() {
        let mut store = store();
        let first = stable(&mut store);
        let refused = store
            .revoke_generation(ADMIN, &first, "too early")
            .unwrap_err()
            .to_string();
        assert!(refused.contains("confirm another one first"), "{refused}");
        let second = stable(&mut store);
        store.revoke_generation(ADMIN, &first, "now").unwrap();
        assert_eq!(status(&store, &first), GenerationStatus::Revoked);
        assert_eq!(status(&store, &second), GenerationStatus::Stable);
    }

    #[test]
    fn the_target_of_a_pending_activation_cannot_be_finished() {
        let mut store = store();
        let id = tested(&mut store);
        let pending = store
            .request_activation(ADMIN, &request(&id, ActivationKind::Activate))
            .unwrap();
        assert_conflict(store.reject_generation(ADMIN, &id, "no"));
        assert_conflict(store.revoke_generation(ADMIN, &id, "no"));
        store
            .confirm_activation(pending.id, &witness(LEDGER_SEQUENCE + 1), "ok")
            .unwrap();
        assert_eq!(status(&store, &id), GenerationStatus::Stable);
    }

    #[test]
    fn the_target_of_a_pending_rollback_cannot_be_revoked() {
        let mut store = store();
        let (old, _new) = superseded(&mut store);
        store
            .request_activation(ADMIN, &request(&old, ActivationKind::Rollback))
            .unwrap();
        assert_conflict(store.revoke_generation(ADMIN, &old, "no"));
        assert_eq!(status(&store, &old), GenerationStatus::Superseded);
    }

    // --- activation requests -------------------------------------------------

    #[test]
    fn a_request_is_durable_and_changes_no_status() {
        let mut store = store();
        let current = stable(&mut store);
        let id = tested(&mut store);
        let pending = store
            .request_activation(Caller::new(77), &request(&id, ActivationKind::Activate))
            .unwrap();
        assert_eq!(pending.state, ActivationState::Pending);
        assert_eq!(pending.actor_uid, 77);
        assert_eq!(pending.previous_generation.as_ref(), Some(&current));
        assert_eq!(status(&store, &id), GenerationStatus::Tested);
        assert_eq!(status(&store, &current), GenerationStatus::Stable);
        assert_eq!(store.pending_activation().unwrap().unwrap().id, pending.id);
        let audit = store.admin_audit_for_object(&ObjectRef::from(&id)).unwrap();
        assert!(audit
            .iter()
            .any(|r| r.command == "generation.activate.request"));
    }

    #[test]
    fn only_one_activation_can_be_pending() {
        let mut store = store();
        let first = tested(&mut store);
        let second = tested(&mut store);
        store
            .request_activation(ADMIN, &request(&first, ActivationKind::Activate))
            .unwrap();
        assert_conflict(
            store.request_activation(ADMIN, &request(&second, ActivationKind::Activate)),
        );
        assert_refused(
            store.conn.execute(
                "INSERT INTO generation_activations
                     (generation, kind, actor_uid, reason, request_digest, expected_slot,
                      expected_root_partuuid, ledger_sequence, ledger_head, requested_at)
                 VALUES (?1, 'ACTIVATE', 0, 'r', ?2, 'root-b', ?3, 0, ?2, 1)",
                params![second.as_str(), hash('f'), PARTUUID],
            ),
            "UNIQUE",
        );
    }

    #[test]
    fn a_request_needs_the_right_status_and_owner() {
        for status_name in ["CANDIDATE", "REJECTED", "REVOKED", "STABLE", "SUPERSEDED"] {
            let mut store = store();
            let id = in_status(&mut store, status_name);
            assert_conflict(
                store.request_activation(ADMIN, &request(&id, ActivationKind::Activate)),
            );
        }
        for status_name in ["CANDIDATE", "REJECTED", "REVOKED", "STABLE", "TESTED"] {
            let mut store = store();
            let id = in_status(&mut store, status_name);
            assert_conflict(
                store.request_activation(ADMIN, &request(&id, ActivationKind::Rollback)),
            );
        }
    }

    #[test]
    fn a_private_tested_generation_cannot_be_activated() {
        let mut store = store();
        let id = candidate_owned(&store, Owner::User(1000));
        make_tested(&mut store, &id);
        assert_conflict(store.request_activation(ADMIN, &request(&id, ActivationKind::Activate)));
    }

    #[test]
    fn a_request_for_a_missing_generation_changes_nothing() {
        let mut store = store();
        let current = stable(&mut store);
        let before = store.activation_history().unwrap().len();
        let missing = GenerationId::new();
        assert!(matches!(
            store.request_activation(ADMIN, &request(&missing, ActivationKind::Activate)),
            Err(StorageError::NotFound(_))
        ));
        assert_eq!(store.activation_history().unwrap().len(), before);
        assert_eq!(status(&store, &current), GenerationStatus::Stable);
    }

    #[test]
    fn requests_are_validated_before_anything_is_written() {
        let mut store = store();
        let id = tested(&mut store);
        let base = request(&id, ActivationKind::Activate);
        let cases: [fn(&mut ActivationRequest); 12] = [
            |r| r.reason = " ".to_owned(),
            |r| r.operation_id = Some(String::new()),
            |r| r.operation_id = Some("a b".to_owned()),
            |r| r.operation_id = Some("a".repeat(MAX_OPERATION_ID + 1)),
            |r| r.expected_slot = "root-c".to_owned(),
            |r| r.expected_root_partuuid = PARTUUID.to_uppercase().replace('1', "A"),
            |r| r.expected_root_partuuid = "not-a-uuid".to_owned(),
            |r| r.expected_root_partuuid = PARTUUID.replace('-', "0"),
            |r| r.ledger_head = "e".repeat(63),
            |r| r.ledger_sequence = u64::MAX,
            |r| r.irreversible = true,
            |r| r.backup_sha256 = Some("xyz".to_owned()),
        ];
        for mutate in cases {
            let mut bad = base.clone();
            mutate(&mut bad);
            assert_invalid(store.request_activation(ADMIN, &bad));
        }
        assert!(store.pending_activation().unwrap().is_none());
    }

    #[test]
    fn the_database_refuses_malformed_activation_rows() {
        let mut store = store();
        let id = tested(&mut store);
        let insert = |kind: &str, actor: i64, slot: &str, uuid: &str, state: &str, irrev: i64| {
            store.conn.execute(
                "INSERT INTO generation_activations
                     (generation, kind, actor_uid, reason, request_digest, expected_slot,
                      expected_root_partuuid, ledger_sequence, ledger_head, state, irreversible,
                      resolved_at, requested_at)
                 VALUES (?1, ?2, ?3, 'r', ?4, ?5, ?6, 0, ?4, ?7, ?8,
                         CASE ?7 WHEN 'PENDING' THEN NULL ELSE 1 END, 1)",
                params![
                    id.as_str(),
                    kind,
                    actor,
                    hash('f'),
                    slot,
                    uuid,
                    state,
                    irrev
                ],
            )
        };
        assert!(insert("UPGRADE", 0, "root-b", PARTUUID, "PENDING", 0).is_err());
        assert!(insert("ACTIVATE", -1, "root-b", PARTUUID, "PENDING", 0).is_err());
        assert!(insert("ACTIVATE", 0, "/dev/sda2", PARTUUID, "PENDING", 0).is_err());
        assert!(insert("ACTIVATE", 0, "root-b", "x", "PENDING", 0).is_err());
        // Irreversible without its backup hash.
        assert!(insert("ACTIVATE", 0, "root-b", PARTUUID, "PENDING", 1).is_err());
        // History cannot start already resolved.
        assert_refused(
            insert("ACTIVATE", 0, "root-b", PARTUUID, "CONFIRMED", 0),
            "starts PENDING",
        );
    }

    #[test]
    fn an_idempotent_retry_returns_the_same_record() {
        let mut store = store();
        let id = tested(&mut store);
        let mut first = request(&id, ActivationKind::Activate);
        first.operation_id = Some("op-1".to_owned());
        let created = store.request_activation(ADMIN, &first).unwrap();

        let mut retry = first.clone();
        retry.reason = "typed differently".to_owned();
        assert_eq!(store.request_activation(ADMIN, &retry).unwrap(), created);
        let audit = store.admin_audit_for_object(&ObjectRef::from(&id)).unwrap();
        assert_eq!(
            audit
                .iter()
                .filter(|r| r.command == "generation.activate.request")
                .count(),
            1
        );

        let resolved = store
            .confirm_activation(created.id, &witness(LEDGER_SEQUENCE + 1), "ok")
            .unwrap();
        assert_eq!(store.request_activation(ADMIN, &first).unwrap(), resolved);
    }

    #[test]
    fn an_operation_id_cannot_be_reused_for_another_request() {
        let mut store = store();
        let id = tested(&mut store);
        let mut first = request(&id, ActivationKind::Activate);
        first.operation_id = Some("op-1".to_owned());
        store.request_activation(ADMIN, &first).unwrap();

        let mut changed = first.clone();
        changed.expected_slot = "root-a".to_owned();
        assert_conflict(store.request_activation(ADMIN, &changed));
        assert_conflict(store.request_activation(Caller::new(5), &first));
        assert_eq!(store.activation_history().unwrap().len(), 1);
    }

    // --- resolution ----------------------------------------------------------

    #[test]
    fn confirmation_moves_stable_in_one_step() {
        let mut store = store();
        let old = stable(&mut store);
        let new = tested(&mut store);
        let pending = store
            .request_activation(ADMIN, &request(&new, ActivationKind::Activate))
            .unwrap();
        let confirmed = store
            .confirm_activation(pending.id, &witness(LEDGER_SEQUENCE + 1), "booted")
            .unwrap();
        assert_eq!(confirmed.state, ActivationState::Confirmed);
        assert_eq!(confirmed.witness_sequence, Some(LEDGER_SEQUENCE + 1));
        assert_eq!(status(&store, &new), GenerationStatus::Stable);
        assert_eq!(status(&store, &old), GenerationStatus::Superseded);
        assert_eq!(store.current_stable().unwrap(), Some(new));
        assert!(store.pending_activation().unwrap().is_none());
    }

    #[test]
    fn rollback_returns_to_a_generation_that_was_stable() {
        let mut store = store();
        let (old, new) = superseded(&mut store);
        let confirmed = activate(&mut store, &old, ActivationKind::Rollback);
        assert_eq!(confirmed.kind, ActivationKind::Rollback);
        assert_eq!(confirmed.previous_generation.as_ref(), Some(&new));
        assert_eq!(status(&store, &old), GenerationStatus::Stable);
        assert_eq!(status(&store, &new), GenerationStatus::Superseded);
    }

    #[test]
    fn a_replayed_witness_cannot_confirm() {
        let mut store = store();
        let id = tested(&mut store);
        let pending = store
            .request_activation(ADMIN, &request(&id, ActivationKind::Activate))
            .unwrap();
        for sequence in [0, LEDGER_SEQUENCE - 1, LEDGER_SEQUENCE] {
            assert_conflict(store.confirm_activation(pending.id, &witness(sequence), "replay"));
            assert_eq!(status(&store, &id), GenerationStatus::Tested);
        }
        assert_eq!(store.pending_activation().unwrap().unwrap().id, pending.id);
    }

    #[test]
    fn the_database_itself_refuses_a_replayed_or_missing_witness() {
        let mut store = store();
        let id = tested(&mut store);
        let pending = store
            .request_activation(ADMIN, &request(&id, ActivationKind::Activate))
            .unwrap();
        for (boot, sequence) in [
            (Some("b"), Some(LEDGER_SEQUENCE as i64)),
            (None, Some(LEDGER_SEQUENCE as i64 + 1)),
            (Some("b"), None),
        ] {
            assert!(store
                .conn
                .execute(
                    "UPDATE generation_activations
                     SET state = 'CONFIRMED', resolved_at = 1, witness_boot_id = ?2,
                         witness_sequence = ?3
                     WHERE id = ?1",
                    params![pending.id, boot, sequence],
                )
                .is_err());
        }
    }

    #[test]
    fn the_database_refuses_a_resolution_without_a_time() {
        let mut store = store();
        let id = tested(&mut store);
        let pending = store
            .request_activation(ADMIN, &request(&id, ActivationKind::Activate))
            .unwrap();
        assert_refused(
            store.conn.execute(
                "UPDATE generation_activations SET state = 'ABANDONED' WHERE id = ?1",
                params![pending.id],
            ),
            "CHECK",
        );
    }

    #[test]
    fn the_database_refuses_a_forged_previous_generation() {
        let mut store = store();
        let current = stable(&mut store);
        let id = tested(&mut store);
        let other = tested(&mut store);
        for previous in [None, Some(other.as_str())] {
            assert_refused(
                store.conn.execute(
                    "INSERT INTO generation_activations
                         (generation, previous_generation, kind, actor_uid, reason,
                          request_digest, expected_slot, expected_root_partuuid,
                          ledger_sequence, ledger_head, requested_at)
                     VALUES (?1, ?2, 'ACTIVATE', 0, 'r', ?3, 'root-b', ?4, 0, ?3, 1)",
                    params![id.as_str(), previous, hash('f'), PARTUUID],
                ),
                "must be the current STABLE",
            );
        }
        assert_eq!(status(&store, &current), GenerationStatus::Stable);
    }

    #[test]
    fn the_database_refuses_a_reused_operation_id() {
        let mut store = store();
        let id = tested(&mut store);
        let mut first = request(&id, ActivationKind::Activate);
        first.operation_id = Some("op-1".to_owned());
        let pending = store.request_activation(ADMIN, &first).unwrap();
        store.abandon_activation(pending.id, "no slot").unwrap();
        assert_refused(
            store.conn.execute(
                "INSERT INTO generation_activations
                     (generation, kind, actor_uid, reason, operation_id, request_digest,
                      expected_slot, expected_root_partuuid, ledger_sequence, ledger_head,
                      requested_at)
                 VALUES (?1, 'ACTIVATE', 0, 'r', 'op-1', ?2, 'root-b', ?3, 0, ?2, 1)",
                params![id.as_str(), hash('f'), PARTUUID],
            ),
            "UNIQUE",
        );
    }

    #[test]
    fn invalid_witnesses_and_details_are_refused() {
        let mut store = store();
        let id = tested(&mut store);
        let pending = store
            .request_activation(ADMIN, &request(&id, ActivationKind::Activate))
            .unwrap();
        let good = witness(LEDGER_SEQUENCE + 1);
        assert_invalid(store.confirm_activation(
            pending.id,
            &Witness {
                boot_id: String::new(),
                sequence: 6,
            },
            "x",
        ));
        assert_invalid(store.confirm_activation(
            pending.id,
            &Witness {
                boot_id: "b".repeat(65),
                sequence: 6,
            },
            "x",
        ));
        assert_invalid(store.confirm_activation(
            pending.id,
            &Witness {
                boot_id: "b".to_owned(),
                sequence: u64::MAX,
            },
            "x",
        ));
        assert_invalid(store.confirm_activation(pending.id, &good, &"d".repeat(MAX_DETAIL + 1)));
        assert_eq!(store.pending_activation().unwrap().unwrap().id, pending.id);
    }

    #[test]
    fn resolving_a_missing_activation_is_not_found() {
        let mut store = store();
        assert!(matches!(
            store.abandon_activation(999, "x"),
            Err(StorageError::NotFound(_))
        ));
    }

    #[test]
    fn repeated_resolution_is_a_no_op() {
        let mut store = store();
        let old = stable(&mut store);
        let id = tested(&mut store);
        let pending = store
            .request_activation(ADMIN, &request(&id, ActivationKind::Activate))
            .unwrap();
        let w = witness(LEDGER_SEQUENCE + 1);
        let first = store.confirm_activation(pending.id, &w, "ok").unwrap();
        let second = store.confirm_activation(pending.id, &w, "ok").unwrap();
        assert_eq!(first, second);
        assert_eq!(status(&store, &id), GenerationStatus::Stable);
        assert_eq!(status(&store, &old), GenerationStatus::Superseded);
    }

    #[test]
    fn a_confirmed_activation_cannot_be_claimed_by_another_witness() {
        let mut store = store();
        let id = tested(&mut store);
        let pending = store
            .request_activation(ADMIN, &request(&id, ActivationKind::Activate))
            .unwrap();
        store
            .confirm_activation(pending.id, &witness(LEDGER_SEQUENCE + 1), "ok")
            .unwrap();
        assert_conflict(store.confirm_activation(pending.id, &witness(LEDGER_SEQUENCE + 2), "ok"));
        let other = Witness {
            boot_id: "boot-2".to_owned(),
            sequence: LEDGER_SEQUENCE + 1,
        };
        assert_conflict(store.confirm_activation(pending.id, &other, "ok"));
    }

    #[test]
    fn a_resolved_activation_cannot_change_outcome() {
        let mut store = store();
        let id = tested(&mut store);
        let pending = store
            .request_activation(ADMIN, &request(&id, ActivationKind::Activate))
            .unwrap();
        store.abandon_activation(pending.id, "no slot").unwrap();
        store.abandon_activation(pending.id, "again").unwrap();
        assert_conflict(store.confirm_activation(pending.id, &witness(LEDGER_SEQUENCE + 1), "x"));
        assert_conflict(store.record_fell_back(pending.id, None, "x"));
        assert_eq!(status(&store, &id), GenerationStatus::Tested);
    }

    #[test]
    fn history_is_append_only() {
        let mut store = store();
        let id = tested(&mut store);
        let pending = store
            .request_activation(ADMIN, &request(&id, ActivationKind::Activate))
            .unwrap();
        for sql in [
            "UPDATE generation_activations SET reason = 'x' WHERE id = ?1",
            "UPDATE generation_activations SET actor_uid = 9 WHERE id = ?1",
            "UPDATE generation_activations SET ledger_sequence = 0 WHERE id = ?1",
            "UPDATE generation_activations SET ledger_head = '' WHERE id = ?1",
            "UPDATE generation_activations SET expected_slot = 'root-a' WHERE id = ?1",
            "UPDATE generation_activations SET irreversible = 1, backup_sha256 = '' WHERE id = ?1",
        ] {
            assert!(
                store.conn.execute(sql, params![pending.id]).is_err(),
                "{sql}"
            );
        }
        assert_refused(
            store.conn.execute(
                "UPDATE generation_activations SET state = 'PENDING' WHERE id = ?1",
                params![pending.id],
            ),
            "resolves to",
        );
        assert_refused(
            store.conn.execute(
                "DELETE FROM generation_activations WHERE id = ?1",
                params![pending.id],
            ),
            "append-only",
        );
        store
            .confirm_activation(pending.id, &witness(LEDGER_SEQUENCE + 1), "ok")
            .unwrap();
        assert_refused(
            store.conn.execute(
                "UPDATE generation_activations SET detail = 'rewritten' WHERE id = ?1",
                params![pending.id],
            ),
            "immutable",
        );
        assert_eq!(store.activation_history().unwrap().len(), 1);
    }

    #[test]
    fn history_lists_every_outcome_in_order() {
        let mut store = store();
        let id = tested(&mut store);
        let abandoned = store
            .request_activation(ADMIN, &request(&id, ActivationKind::Activate))
            .unwrap();
        store.abandon_activation(abandoned.id, "no slot").unwrap();
        let confirmed = activate(&mut store, &id, ActivationKind::Activate);
        let history = store.activation_history().unwrap();
        assert_eq!(
            history.iter().map(|a| a.state).collect::<Vec<_>>(),
            vec![ActivationState::Abandoned, ActivationState::Confirmed]
        );
        assert_eq!(store.activation(confirmed.id).unwrap().unwrap(), confirmed);
    }

    // --- FELL_BACK needs a new explicit decision (Q4) ------------------------

    fn fell_back(store: &mut Store, id: &GenerationId, ack: Option<i64>) -> i64 {
        let mut req = request(id, ActivationKind::Activate);
        req.acknowledges = ack;
        let pending = store.request_activation(ADMIN, &req).unwrap();
        store
            .record_fell_back(
                pending.id,
                Some(&witness(LEDGER_SEQUENCE + 1)),
                "previous slot came back",
            )
            .unwrap();
        pending.id
    }

    fn with_ack(id: &GenerationId, ack: Option<i64>) -> ActivationRequest {
        let mut req = request(id, ActivationKind::Activate);
        req.acknowledges = ack;
        req
    }

    #[test]
    fn fell_back_keeps_tested_and_records_why() {
        let mut store = store();
        let id = tested(&mut store);
        let row = fell_back(&mut store, &id, None);
        assert_eq!(status(&store, &id), GenerationStatus::Tested);
        let record = store.activation(row).unwrap().unwrap();
        assert_eq!(record.state, ActivationState::FellBack);
        assert_eq!(record.detail.as_deref(), Some("previous slot came back"));
    }

    #[test]
    fn reactivation_after_fell_back_needs_an_acknowledgement() {
        let mut store = store();
        let id = tested(&mut store);
        let row = fell_back(&mut store, &id, None);
        assert_conflict(store.request_activation(ADMIN, &with_ack(&id, None)));
        let next = store
            .request_activation(ADMIN, &with_ack(&id, Some(row)))
            .unwrap();
        assert_eq!(next.acknowledges, Some(row));
    }

    #[test]
    fn an_acknowledgement_must_name_a_fell_back_row_of_this_generation() {
        let mut store = store();
        let id = tested(&mut store);
        let row = fell_back(&mut store, &id, None);
        let other = tested(&mut store);
        let other_row = fell_back(&mut store, &other, None);
        assert_conflict(store.request_activation(ADMIN, &with_ack(&id, Some(other_row))));
        assert!(store
            .request_activation(ADMIN, &with_ack(&id, Some(9999)))
            .is_err());

        // A Generation with nothing to acknowledge cannot borrow another's row.
        let clean = tested(&mut store);
        assert_conflict(store.request_activation(ADMIN, &with_ack(&clean, Some(other_row))));

        let abandoned = store
            .request_activation(ADMIN, &with_ack(&id, Some(row)))
            .unwrap();
        store.abandon_activation(abandoned.id, "no slot").unwrap();
        // An abandoned row is not something that can be acknowledged.
        assert_conflict(store.request_activation(ADMIN, &with_ack(&id, Some(abandoned.id))));
    }

    #[test]
    fn an_abandoned_retry_does_not_use_up_the_requirement() {
        let mut store = store();
        let id = tested(&mut store);
        let row = fell_back(&mut store, &id, None);
        let retry = store
            .request_activation(ADMIN, &with_ack(&id, Some(row)))
            .unwrap();
        store.abandon_activation(retry.id, "no slot").unwrap();
        assert_conflict(store.request_activation(ADMIN, &with_ack(&id, None)));
        store
            .request_activation(ADMIN, &with_ack(&id, Some(row)))
            .unwrap();
    }

    #[test]
    fn each_new_fall_back_needs_its_own_acknowledgement() {
        let mut store = store();
        let id = tested(&mut store);
        let first = fell_back(&mut store, &id, None);
        let second = fell_back(&mut store, &id, Some(first));
        assert_conflict(store.request_activation(ADMIN, &with_ack(&id, None)));
        assert_conflict(store.request_activation(ADMIN, &with_ack(&id, Some(first))));
        store
            .request_activation(ADMIN, &with_ack(&id, Some(second)))
            .unwrap();
    }

    // --- failure injection and restart ---------------------------------------

    fn file_store() -> (tempfile::TempDir, std::path::PathBuf, Store) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let mut store = Store::open(&path).unwrap();
        store.migrate().unwrap();
        (dir, path, store)
    }

    #[test]
    fn a_failure_at_every_step_of_confirmation_leaves_the_old_state() {
        for (name, table_event) in [
            (
                "activation row",
                "BEFORE UPDATE ON generation_activations WHEN NEW.state = 'CONFIRMED'",
            ),
            (
                "activation row done",
                "AFTER UPDATE ON generation_activations WHEN NEW.state = 'CONFIRMED'",
            ),
            (
                "supersede",
                "BEFORE UPDATE OF status ON generations WHEN NEW.status = 'SUPERSEDED'",
            ),
            (
                "supersede done",
                "AFTER UPDATE OF status ON generations WHEN NEW.status = 'SUPERSEDED'",
            ),
            (
                "stable",
                "BEFORE UPDATE OF status ON generations WHEN NEW.status = 'STABLE'",
            ),
            (
                "stable done",
                "AFTER UPDATE OF status ON generations WHEN NEW.status = 'STABLE'",
            ),
        ] {
            let (_dir, path, mut store) = file_store();
            let old = stable(&mut store);
            let new = tested(&mut store);
            let pending = store
                .request_activation(ADMIN, &request(&new, ActivationKind::Activate))
                .unwrap();
            store
                .conn
                .execute_batch(&format!(
                    "CREATE TRIGGER inject_failure {table_event}
                     BEGIN SELECT RAISE(ABORT, 'injected failure'); END;"
                ))
                .unwrap();
            let w = witness(LEDGER_SEQUENCE + 1);
            assert!(
                store.confirm_activation(pending.id, &w, "ok").is_err(),
                "{name}"
            );
            drop(store);

            let mut store = Store::open(&path).unwrap();
            assert_eq!(status(&store, &new), GenerationStatus::Tested, "{name}");
            assert_eq!(status(&store, &old), GenerationStatus::Stable, "{name}");
            assert_eq!(
                store.pending_activation().unwrap().unwrap().id,
                pending.id,
                "{name}"
            );

            store
                .conn
                .execute_batch("DROP TRIGGER inject_failure")
                .unwrap();
            store.confirm_activation(pending.id, &w, "ok").unwrap();
            assert_eq!(status(&store, &new), GenerationStatus::Stable, "{name}");
            assert_eq!(status(&store, &old), GenerationStatus::Superseded, "{name}");
        }
    }

    #[test]
    fn a_failure_while_requesting_writes_nothing() {
        let (_dir, path, mut store) = file_store();
        let id = tested(&mut store);
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER inject_failure BEFORE INSERT ON admin_audit
                 BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
            )
            .unwrap();
        assert!(store
            .request_activation(ADMIN, &request(&id, ActivationKind::Activate))
            .is_err());
        drop(store);
        let store = Store::open(&path).unwrap();
        assert!(store.activation_history().unwrap().is_empty());
    }

    #[test]
    fn a_failure_while_declaring_tested_writes_nothing() {
        let (_dir, _path, mut store) = file_store();
        let id = candidate(&store);
        store
            .set_root_identity(ADMIN, &id, COMMIT, &identity(), "built")
            .unwrap();
        let evidence = evidence_for(&store, &id);
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER inject_failure BEFORE INSERT ON admin_audit
                 WHEN NEW.command = 'generation.test'
                 BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
            )
            .unwrap();
        assert!(store.declare_tested(ADMIN, &id, &evidence, "x").is_err());
        assert_eq!(status(&store, &id), GenerationStatus::Candidate);
        assert!(store
            .provenance_from(&ObjectRef::from(&id))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn activation_state_survives_a_restart_and_resolves_once() {
        let (_dir, path, mut store) = file_store();
        let old = stable(&mut store);
        let new = tested(&mut store);
        let pending = store
            .request_activation(ADMIN, &request(&new, ActivationKind::Activate))
            .unwrap();
        drop(store);

        let mut store = Store::open(&path).unwrap();
        assert_eq!(store.pending_activation().unwrap().unwrap(), pending);
        let w = witness(LEDGER_SEQUENCE + 1);
        let confirmed = store.confirm_activation(pending.id, &w, "ok").unwrap();
        drop(store);

        // The process died after the commit, before anyone heard the answer.
        let mut store = Store::open(&path).unwrap();
        assert_eq!(
            store.confirm_activation(pending.id, &w, "ok").unwrap(),
            confirmed
        );
        assert_eq!(status(&store, &new), GenerationStatus::Stable);
        assert_eq!(status(&store, &old), GenerationStatus::Superseded);
        assert_eq!(store.activation_history().unwrap().len(), 2);
    }

    // --- witness resolution (G3) ---------------------------------------------

    use crate::witness::parse_ledger;
    use crate::witness::testing::{witness_json, Chain, BOOT_1, BOOT_2, BOOT_3};

    /// A STABLE Generation, a TESTED one to activate on root-b, and the PENDING
    /// request recorded against the ledger as it was then.
    fn pending_on(store: &mut Store, chain: &Chain) -> (GenerationId, GenerationId, Activation) {
        let old = stable(store);
        let new = tested(store);
        let at = chain.parse();
        let mut req = request(&new, ActivationKind::Activate);
        req.ledger_sequence = at.last_sequence();
        req.ledger_head = at.head().to_owned();
        let pending = store.request_activation(ADMIN, &req).unwrap();
        (old, new, pending)
    }

    fn booted_a() -> Chain {
        let mut chain = Chain::default();
        chain.witness(BOOT_1, "root-a").health(BOOT_1);
        chain
    }

    #[test]
    fn nothing_pending_resolves_to_nothing() {
        let mut store = store();
        assert_eq!(
            store
                .resolve_pending_activation(&booted_a().parse(), BOOT_1)
                .unwrap(),
            Resolution::NoPending
        );
    }

    #[test]
    fn a_healthy_witnessed_target_boot_makes_it_stable_once() {
        let mut store = store();
        let mut chain = booted_a();
        let (old, new, pending) = pending_on(&mut store, &chain);
        chain.witness(BOOT_2, "root-b").health(BOOT_2);
        let ledger = chain.parse();
        match store.resolve_pending_activation(&ledger, BOOT_2).unwrap() {
            Resolution::Resolved(a) => {
                assert_eq!((a.id, a.state), (pending.id, ActivationState::Confirmed));
                assert_eq!(a.witness_boot_id.as_deref(), Some(BOOT_2));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(status(&store, &new), GenerationStatus::Stable);
        assert_eq!(status(&store, &old), GenerationStatus::Superseded);
        assert_eq!(
            store.resolve_pending_activation(&ledger, BOOT_2).unwrap(),
            Resolution::NoPending
        );
    }

    #[test]
    fn waiting_and_unresolved_leave_the_request_untouched() {
        let mut store = store();
        let mut chain = booted_a();
        let (old, new, pending) = pending_on(&mut store, &chain);
        assert!(matches!(
            store
                .resolve_pending_activation(&chain.parse(), BOOT_1)
                .unwrap(),
            Resolution::Waiting { .. }
        ));
        chain.witness(BOOT_2, "root-b");
        assert!(matches!(
            store
                .resolve_pending_activation(&chain.parse(), BOOT_2)
                .unwrap(),
            Resolution::Waiting { .. }
        ));
        chain.health(BOOT_2);
        // A copied ledger read under another kernel proves nothing.
        assert!(matches!(
            store
                .resolve_pending_activation(&chain.parse(), BOOT_3)
                .unwrap(),
            Resolution::Unresolved { .. }
        ));
        assert_eq!(store.pending_activation().unwrap().unwrap(), pending);
        assert_eq!(status(&store, &new), GenerationStatus::Tested);
        assert_eq!(status(&store, &old), GenerationStatus::Stable);
    }

    #[test]
    fn a_fall_back_keeps_tested_and_needs_a_new_decision() {
        let mut store = store();
        let mut chain = booted_a();
        let (old, new, pending) = pending_on(&mut store, &chain);
        chain
            .witness(BOOT_2, "root-b")
            .witness(BOOT_3, "root-a")
            .health(BOOT_3);
        match store
            .resolve_pending_activation(&chain.parse(), BOOT_3)
            .unwrap()
        {
            Resolution::Resolved(a) => assert_eq!(a.state, ActivationState::FellBack),
            other => panic!("{other:?}"),
        }
        assert_eq!(status(&store, &new), GenerationStatus::Tested);
        assert_eq!(status(&store, &old), GenerationStatus::Stable);
        assert_conflict(store.request_activation(ADMIN, &with_ack(&new, None)));
        store
            .request_activation(ADMIN, &with_ack(&new, Some(pending.id)))
            .unwrap();
    }

    #[test]
    fn a_target_boot_of_another_image_is_abandoned_without_stable() {
        let mut store = store();
        let mut chain = booted_a();
        let (old, new, _) = pending_on(&mut store, &chain);
        // A witness of root-b, but built from another input manifest.
        let mut record = witness_json(BOOT_2, "root-b", "b", PARTUUID);
        record["input_manifest_sha256"] = "f".repeat(64).into();
        chain.push(record).health(BOOT_2);
        match store
            .resolve_pending_activation(&chain.parse(), BOOT_2)
            .unwrap()
        {
            Resolution::Resolved(a) => assert_eq!(a.state, ActivationState::Abandoned),
            other => panic!("{other:?}"),
        }
        assert_eq!(status(&store, &new), GenerationStatus::Tested);
        assert_eq!(status(&store, &old), GenerationStatus::Stable);
    }

    /// The ledger and boot id as files, as the daemon reads them.
    fn ledger_files(
        dir: &std::path::Path,
        chain: &Chain,
        boot: &str,
    ) -> crate::witness::LedgerSource {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let path = dir.join("boot-witness.jsonl");
        std::fs::write(&path, chain.bytes()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let boot_id_path = dir.join("boot_id");
        std::fs::write(&boot_id_path, format!("{boot}\n")).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        crate::witness::LedgerSource {
            path,
            policy: crate::witness::LedgerPolicy {
                owner_uid: meta.uid(),
                reader_gid: meta.gid(),
            },
            boot_id_path,
        }
    }

    #[test]
    fn nothing_pending_reads_no_ledger() {
        let mut store = store();
        let source = crate::witness::LedgerSource {
            path: "/nonexistent/boot-witness.jsonl".into(),
            policy: crate::witness::LedgerPolicy {
                owner_uid: 0,
                reader_gid: 0,
            },
            boot_id_path: "/nonexistent/boot_id".into(),
        };
        assert_eq!(
            crate::witness::resolve_from(&mut store, &source).unwrap(),
            Resolution::NoPending
        );
    }

    #[test]
    fn resolving_from_the_files_confirms_a_healthy_live_boot() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store();
        let mut chain = booted_a();
        let (old, new, _) = pending_on(&mut store, &chain);
        chain.witness(BOOT_2, "root-b").health(BOOT_2);
        let source = ledger_files(dir.path(), &chain, BOOT_2);
        assert!(matches!(
            crate::witness::resolve_from(&mut store, &source).unwrap(),
            Resolution::Resolved(ref a) if a.state == ActivationState::Confirmed
        ));
        assert_eq!(status(&store, &new), GenerationStatus::Stable);
        assert_eq!(status(&store, &old), GenerationStatus::Superseded);
    }

    #[test]
    fn unreadable_files_leave_the_request_pending() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store();
        let mut chain = booted_a();
        let (_, new, pending) = pending_on(&mut store, &chain);
        chain.witness(BOOT_2, "root-b").health(BOOT_2);
        let good = ledger_files(dir.path(), &chain, BOOT_2);
        let mut missing = good.clone();
        missing.path = dir.path().join("absent.jsonl");
        let mut no_boot_id = good.clone();
        no_boot_id.boot_id_path = dir.path().join("absent");
        let mut wrong_owner = good.clone();
        wrong_owner.policy.owner_uid += 1;
        for source in [missing, no_boot_id, wrong_owner] {
            assert!(matches!(
                crate::witness::resolve_from(&mut store, &source).unwrap(),
                Resolution::Unresolved { activation, .. } if activation == pending.id
            ));
        }
        // A tampered ledger on disk.
        let mut tampered = chain.bytes();
        let at = tampered.iter().position(|b| *b == b'1').unwrap();
        tampered[at] = b'9';
        std::fs::write(&good.path, tampered).unwrap();
        match crate::witness::resolve_from(&mut store, &good).unwrap() {
            Resolution::Unresolved { reason, .. } => {
                assert!(reason.contains("refused"), "{reason}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(status(&store, &new), GenerationStatus::Tested);
        assert!(store.pending_activation().unwrap().is_some());
    }

    const CRASH_DB: &str = "FLOWERLY_TEST_CRASH_DB";
    const CRASH_LEDGER: &str = "FLOWERLY_TEST_CRASH_LEDGER";

    /// Does nothing unless run as the child of the crash test below.
    #[test]
    fn crash_child() {
        let (Ok(db), Ok(ledger)) = (std::env::var(CRASH_DB), std::env::var(CRASH_LEDGER)) else {
            return;
        };
        let mut store = Store::open(std::path::Path::new(&db)).unwrap();
        let ledger = parse_ledger(&std::fs::read(ledger).unwrap()).unwrap();
        store.resolve_pending_activation(&ledger, BOOT_2).unwrap();
        panic!("the crash point was not reached");
    }

    #[test]
    fn killing_the_process_at_every_point_of_resolution_leaves_old_or_new_state() {
        use std::os::unix::process::ExitStatusExt;
        for (point, committed) in [
            ("resolve:begin", false),
            ("resolve:activation", false),
            ("resolve:superseded", false),
            ("resolve:stable", false),
            ("resolve:committed", true),
        ] {
            let (dir, path, mut store) = file_store();
            let mut chain = booted_a();
            let (old, new, pending) = pending_on(&mut store, &chain);
            chain.witness(BOOT_2, "root-b").health(BOOT_2);
            let ledger_path = dir.path().join("boot-witness.jsonl");
            std::fs::write(&ledger_path, chain.bytes()).unwrap();
            drop(store);

            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "storage::lifecycle::tests::crash_child",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env(CRASH_DB, &path)
                .env(CRASH_LEDGER, &ledger_path)
                .env("FLOWERLY_TEST_CRASH_AT", point)
                .output()
                .unwrap();
            assert_eq!(
                output.status.signal(),
                Some(6),
                "{point}: the child must die by SIGABRT: {}",
                String::from_utf8_lossy(&output.stderr)
            );

            let mut store = Store::open(&path).unwrap();
            let (want_new, want_old) = if committed {
                (GenerationStatus::Stable, GenerationStatus::Superseded)
            } else {
                (GenerationStatus::Tested, GenerationStatus::Stable)
            };
            assert_eq!(status(&store, &new), want_new, "{point}");
            assert_eq!(status(&store, &old), want_old, "{point}");
            assert_eq!(
                store.pending_activation().unwrap().is_some(),
                !committed,
                "{point}"
            );

            let ledger = chain.parse();
            let first = store.resolve_pending_activation(&ledger, BOOT_2).unwrap();
            if committed {
                assert_eq!(first, Resolution::NoPending, "{point}");
            } else {
                assert!(
                    matches!(first, Resolution::Resolved(ref a) if a.id == pending.id),
                    "{point}"
                );
            }
            assert_eq!(
                store.resolve_pending_activation(&ledger, BOOT_2).unwrap(),
                Resolution::NoPending,
                "{point}"
            );
            assert_eq!(status(&store, &new), GenerationStatus::Stable, "{point}");
            assert_eq!(
                status(&store, &old),
                GenerationStatus::Superseded,
                "{point}"
            );
            let history = store.activation_history().unwrap();
            assert_eq!(
                history.last().unwrap().state,
                ActivationState::Confirmed,
                "{point}"
            );
        }
    }

    // --- legacy databases ----------------------------------------------------

    /// A database as installed just before this migration.
    fn store_before_lifecycle() -> Store {
        let store = Store::open_in_memory().expect("open store");
        store
            .conn
            .execute_batch(
                "CREATE TABLE schema_migrations (
                     version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at INTEGER NOT NULL
                 );",
            )
            .unwrap();
        for migration in MIGRATIONS.iter().filter(|m| m.version < 15) {
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

    fn insert_legacy(store: &Store, id: &str, status: &str) {
        store
            .conn
            .execute(
                "INSERT INTO generations (id, status, created_at, owner_class, source_commit)
                 VALUES (?1, ?2, 1, 'system', ?3)",
                params![id, status, COMMIT],
            )
            .unwrap();
    }

    fn legacy_id(id: &str) -> GenerationId {
        id.parse().unwrap()
    }

    #[test]
    fn legacy_rows_are_validated_and_not_rewritten() {
        let mut store = store_before_lifecycle();
        for (id, state) in [
            ("gen-cand", "CANDIDATE"),
            ("gen-test", "TESTED"),
            ("gen-rej", "REJECTED"),
            ("gen-stable", "STABLE"),
            ("gen-old", "SUPERSEDED"),
        ] {
            insert_legacy(&store, id, state);
        }
        store.migrate().unwrap();
        for (id, state) in [
            ("gen-cand", "CANDIDATE"),
            ("gen-test", "TESTED"),
            ("gen-rej", "REJECTED"),
            ("gen-stable", "STABLE"),
            ("gen-old", "SUPERSEDED"),
        ] {
            assert_eq!(status(&store, &legacy_id(id)).to_string(), state);
        }
    }

    #[test]
    fn a_legacy_database_with_two_stable_rows_is_refused_unchanged() {
        let mut store = store_before_lifecycle();
        insert_legacy(&store, "gen-one", "STABLE");
        insert_legacy(&store, "gen-two", "STABLE");
        assert!(store.migrate().is_err());
        assert_eq!(
            store.applied_migrations().unwrap().last().unwrap().version,
            14
        );
        for id in ["gen-one", "gen-two"] {
            assert_eq!(status(&store, &legacy_id(id)), GenerationStatus::Stable);
        }
        let tables: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'generation_activations'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
    }

    #[test]
    fn a_legacy_stable_is_replaced_by_the_first_confirmation() {
        let mut store = store_before_lifecycle();
        insert_legacy(&store, "gen-stable", "STABLE");
        store.migrate().unwrap();
        let new = tested(&mut store);
        let confirmed = activate(&mut store, &new, ActivationKind::Activate);
        assert_eq!(confirmed.previous_generation, Some(legacy_id("gen-stable")));
        assert_eq!(
            status(&store, &legacy_id("gen-stable")),
            GenerationStatus::Superseded
        );
        assert_eq!(status(&store, &new), GenerationStatus::Stable);
    }

    #[test]
    fn a_legacy_tested_row_has_no_identity_and_cannot_be_activated() {
        let mut store = store_before_lifecycle();
        insert_legacy(&store, "gen-test", "TESTED");
        store.migrate().unwrap();
        let id = legacy_id("gen-test");
        assert_conflict(store.request_activation(ADMIN, &request(&id, ActivationKind::Activate)));
        assert_conflict(store.set_root_identity(ADMIN, &id, COMMIT, &identity(), "late"));
    }

    #[test]
    fn rollback_needs_a_generation_that_was_confirmed_stable() {
        let mut store = store_before_lifecycle();
        insert_legacy(&store, "gen-old", "SUPERSEDED");
        insert_legacy(&store, "gen-stable", "STABLE");
        store.migrate().unwrap();
        let id = legacy_id("gen-old");
        assert_conflict(store.request_activation(ADMIN, &request(&id, ActivationKind::Rollback)));

        // Even with a complete identity forced in, no confirmation means no rollback.
        store
            .conn
            .execute_batch("DROP TRIGGER generation_identities_insert_candidate_only")
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO generation_identities
                     (generation, rootfs_sha256, kernel_sha256, initramfs_sha256,
                      input_manifest_sha256, build_id)
                 VALUES ('gen-old', ?1, ?1, ?1, ?1, 'b')",
                params![hash('a')],
            )
            .unwrap();
        let error = store
            .request_activation(ADMIN, &request(&id, ActivationKind::Rollback))
            .unwrap_err()
            .to_string();
        assert!(error.contains("confirmed STABLE before"), "{error}");
    }

    #[test]
    fn checking_a_request_runs_every_guard_and_writes_nothing() {
        let mut store = store();
        let id = tested(&mut store);
        let audited = |store: &Store| -> i64 {
            store
                .conn
                .query_row(
                    "SELECT count(*) FROM admin_audit WHERE command = 'generation.activate.request'",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        };
        store
            .check_activation_request(ADMIN, &request(&id, ActivationKind::Activate))
            .unwrap();
        assert!(store.activation_history().unwrap().is_empty());
        assert_eq!(audited(&store), 0);
        // The guards of the real insert apply: a CANDIDATE, a ROLLBACK to a
        // never-STABLE Generation, an invalid request, a second PENDING.
        let fresh = candidate(&store);
        for (generation, kind) in [
            (&fresh, ActivationKind::Activate),
            (&id, ActivationKind::Rollback),
        ] {
            assert!(store
                .check_activation_request(ADMIN, &request(generation, kind))
                .is_err());
        }
        let mut bad = request(&id, ActivationKind::Activate);
        bad.irreversible = true;
        assert!(store.check_activation_request(ADMIN, &bad).is_err());
        store
            .request_activation(ADMIN, &request(&id, ActivationKind::Activate))
            .unwrap();
        let other = tested(&mut store);
        let error = store
            .check_activation_request(ADMIN, &request(&other, ActivationKind::Activate))
            .unwrap_err()
            .to_string();
        assert!(error.contains("PENDING"), "{error}");
        assert_eq!(store.activation_history().unwrap().len(), 1);
        assert_eq!(audited(&store), 1);
    }

    #[test]
    fn an_activation_is_found_by_its_operation_id() {
        let mut store = store();
        let id = tested(&mut store);
        let mut keyed = request(&id, ActivationKind::Activate);
        keyed.operation_id = Some("op-1".to_owned());
        let created = store.request_activation(ADMIN, &keyed).unwrap();
        assert_eq!(
            store
                .activation_for_operation("op-1")
                .unwrap()
                .map(|a| a.id),
            Some(created.id)
        );
        assert!(store.activation_for_operation("op-2").unwrap().is_none());
    }
}
