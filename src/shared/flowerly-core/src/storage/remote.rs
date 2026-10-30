//! Remote inference consent grants and attempts (ADR-0022, migration 0014).
//!
//! Both are owner-scoped like workloads: another uid's rows are
//! indistinguishable from missing ones. Attempts hold metadata only (owner
//! Q4); no method accepts prompt or response text.
use rusqlite::{params, OptionalExtension, Row, TransactionBehavior};

use super::{Result, StorageError, Store};
use crate::types::{Caller, Timestamp};

const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentRecord {
    pub id: String,
    pub provider_id: String,
    pub destination: String,
    pub granted_at: i64,
    pub expires_at: i64,
    pub revoked_at: Option<i64>,
}

impl ConsentRecord {
    #[must_use]
    pub fn active_at(&self, now: i64) -> bool {
        self.revoked_at.is_none() && now < self.expires_at
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptState {
    /// Recorded before the provider was asked; the outcome of the submit
    /// itself is not yet known.
    Submitting,
    Pending,
    Succeeded,
    Failed,
    /// The request may have executed (and been billed). Final.
    Unknown,
}

impl AttemptState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Submitting => "submitting",
            Self::Pending => "pending",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }

    fn parse(raw: &str) -> rusqlite::Result<Self> {
        Ok(match raw {
            "submitting" => Self::Submitting,
            "pending" => Self::Pending,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "unknown" => Self::Unknown,
            other => {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    format!("unknown attempt state `{other}`").into(),
                ))
            }
        })
    }

    #[must_use]
    pub fn is_final(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Unknown)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptRecord {
    pub id: String,
    pub workload_id: String,
    pub provider_id: String,
    pub destination: String,
    pub model: String,
    pub consent_id: String,
    pub operation_id: Option<String>,
    pub request_id: String,
    pub request_digest: String,
    pub state: AttemptState,
    pub outcome: Option<String>,
    pub error_class: Option<String>,
    pub model_used: Option<String>,
    pub usage_source: Option<String>,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    pub latency_ms: Option<u64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// How an attempt finished, as reported by the provider.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttemptUpdate {
    pub outcome: Option<String>,
    pub error_class: Option<String>,
    pub model_used: Option<String>,
    pub usage_source: Option<String>,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    pub latency_ms: Option<u64>,
}

const CONSENT_COLUMNS: &str = "id, provider_id, destination, granted_at, expires_at, revoked_at";
const ATTEMPT_COLUMNS: &str = "id, workload_id, provider_id, destination, model, consent_id,
     operation_id, request_id, request_digest, state, outcome, error_class, model_used,
     usage_source, tokens_in, tokens_out, latency_ms, created_at, updated_at";

fn map_consent(row: &Row<'_>) -> rusqlite::Result<ConsentRecord> {
    Ok(ConsentRecord {
        id: row.get(0)?,
        provider_id: row.get(1)?,
        destination: row.get(2)?,
        granted_at: row.get(3)?,
        expires_at: row.get(4)?,
        revoked_at: row.get(5)?,
    })
}

fn opt_u64(value: Option<i64>) -> Option<u64> {
    value.and_then(|raw| u64::try_from(raw).ok())
}

fn map_attempt(row: &Row<'_>) -> rusqlite::Result<AttemptRecord> {
    Ok(AttemptRecord {
        id: row.get(0)?,
        workload_id: row.get(1)?,
        provider_id: row.get(2)?,
        destination: row.get(3)?,
        model: row.get(4)?,
        consent_id: row.get(5)?,
        operation_id: row.get(6)?,
        request_id: row.get(7)?,
        request_digest: row.get(8)?,
        state: AttemptState::parse(&row.get::<_, String>(9)?)?,
        outcome: row.get(10)?,
        error_class: row.get(11)?,
        model_used: row.get(12)?,
        usage_source: row.get(13)?,
        tokens_in: opt_u64(row.get(14)?),
        tokens_out: opt_u64(row.get(15)?),
        latency_ms: opt_u64(row.get(16)?),
        created_at: row.get(17)?,
        updated_at: row.get(18)?,
    })
}

fn to_i64(value: Option<u64>) -> Option<i64> {
    value.map(|raw| i64::try_from(raw).unwrap_or(i64::MAX))
}

/// A new attempt, written before the provider is asked (`Submitting`).
pub struct NewAttempt<'a> {
    pub id: &'a str,
    pub workload_id: &'a str,
    pub provider_id: &'a str,
    pub destination: &'a str,
    pub model: &'a str,
    pub consent_id: &'a str,
    pub operation_id: Option<&'a str>,
    pub request_id: &'a str,
    pub request_digest: &'a str,
}

impl Store {
    /// Grant consent for `days` (1..=30) to send this user's prompts to
    /// `destination` through `provider_id`.
    pub fn grant_remote_consent(
        &mut self,
        caller: Caller,
        provider_id: &str,
        destination: &str,
        days: u64,
    ) -> Result<ConsentRecord> {
        if !(1..=crate::remote_policy::MAX_CONSENT_DAYS).contains(&days) {
            return Err(StorageError::InvalidOperation(format!(
                "consent lasts 1..={} days",
                crate::remote_policy::MAX_CONSENT_DAYS
            )));
        }
        let now = Timestamp::now().as_millis();
        let record = ConsentRecord {
            id: format!("consent-{}", uuid::Uuid::new_v4().simple()),
            provider_id: provider_id.to_owned(),
            destination: destination.to_owned(),
            granted_at: now,
            expires_at: now + i64::try_from(days).unwrap_or(30) * DAY_MS,
            revoked_at: None,
        };
        self.conn.execute(
            "INSERT INTO remote_inference_consents
                 (id, owner_class, owner_uid, provider_id, destination, granted_at, expires_at)
             VALUES (?1, 'user', ?2, ?3, ?4, ?5, ?6)",
            params![
                record.id,
                caller.uid(),
                record.provider_id,
                record.destination,
                record.granted_at,
                record.expires_at
            ],
        )?;
        Ok(record)
    }

    /// Revoke the caller's active grants: one by id, or all for a provider.
    /// Returns how many were revoked.
    pub fn revoke_remote_consent(
        &mut self,
        caller: Caller,
        consent_id: Option<&str>,
        provider_id: Option<&str>,
    ) -> Result<usize> {
        let now = Timestamp::now().as_millis();
        Ok(self.conn.execute(
            "UPDATE remote_inference_consents SET revoked_at = ?2
             WHERE owner_class = 'user' AND owner_uid = ?1 AND revoked_at IS NULL
               AND (?3 IS NULL OR id = ?3) AND (?4 IS NULL OR provider_id = ?4)",
            params![caller.uid(), now, consent_id, provider_id],
        )?)
    }

    pub fn list_remote_consents(&self, caller: Caller) -> Result<Vec<ConsentRecord>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {CONSENT_COLUMNS} FROM remote_inference_consents
             WHERE owner_class = 'user' AND owner_uid = ?1 ORDER BY granted_at DESC, id"
        ))?;
        let rows = statement.query_map(params![caller.uid()], map_consent)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The caller's newest grant that is unrevoked, unexpired and scoped to
    /// exactly this provider and destination.
    pub fn active_remote_consent(
        &self,
        caller: Caller,
        provider_id: &str,
        destination: &str,
        now: i64,
    ) -> Result<Option<ConsentRecord>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {CONSENT_COLUMNS} FROM remote_inference_consents
                     WHERE owner_class = 'user' AND owner_uid = ?1 AND provider_id = ?2
                       AND destination = ?3 AND revoked_at IS NULL AND expires_at > ?4
                     ORDER BY expires_at DESC LIMIT 1"
                ),
                params![caller.uid(), provider_id, destination, now],
                map_consent,
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// Record an attempt before anything is sent. A second attempt for the
    /// same user and operation_id is an `OperationConflict`.
    pub fn insert_remote_attempt(
        &mut self,
        caller: Caller,
        attempt: &NewAttempt<'_>,
    ) -> Result<()> {
        let now = Timestamp::now().as_millis();
        let result = self.conn.execute(
            "INSERT INTO remote_inference_attempts
                 (id, owner_class, owner_uid, workload_id, provider_id, destination, model,
                  consent_id, operation_id, request_id, request_digest, state, created_at,
                  updated_at)
             VALUES (?1, 'user', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'submitting', ?11, ?11)",
            params![
                attempt.id,
                caller.uid(),
                attempt.workload_id,
                attempt.provider_id,
                attempt.destination,
                attempt.model,
                attempt.consent_id,
                attempt.operation_id,
                attempt.request_id,
                attempt.request_digest,
                now
            ],
        );
        match result {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::ConstraintViolation
                    && attempt.operation_id.is_some() =>
            {
                Err(StorageError::OperationConflict(
                    "operation_id already names a remote inference attempt".into(),
                ))
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn remote_attempt_for(&self, caller: Caller, id: &str) -> Result<Option<AttemptRecord>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {ATTEMPT_COLUMNS} FROM remote_inference_attempts
                     WHERE owner_class = 'user' AND owner_uid = ?1 AND id = ?2"
                ),
                params![caller.uid(), id],
                map_attempt,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn remote_attempt_by_operation(
        &self,
        caller: Caller,
        operation_id: &str,
    ) -> Result<Option<AttemptRecord>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {ATTEMPT_COLUMNS} FROM remote_inference_attempts
                     WHERE owner_class = 'user' AND owner_uid = ?1 AND operation_id = ?2"
                ),
                params![caller.uid(), operation_id],
                map_attempt,
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// The caller's attempts still awaiting an outcome at one provider.
    pub fn count_open_remote_attempts(&self, caller: Caller, provider_id: &str) -> Result<u32> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM remote_inference_attempts
             WHERE owner_class = 'user' AND owner_uid = ?1 AND provider_id = ?2
               AND state IN ('submitting', 'pending')",
            params![caller.uid(), provider_id],
            |row| row.get(0),
        )?)
    }

    /// Move an open attempt to `state`. Final attempts never change (also
    /// enforced by a trigger); updating one is a `Conflict`.
    pub fn update_remote_attempt(
        &mut self,
        caller: Caller,
        id: &str,
        state: AttemptState,
        update: &AttemptUpdate,
    ) -> Result<AttemptRecord> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<AttemptRecord> = tx
            .query_row(
                &format!(
                    "SELECT {ATTEMPT_COLUMNS} FROM remote_inference_attempts
                     WHERE owner_class = 'user' AND owner_uid = ?1 AND id = ?2"
                ),
                params![caller.uid(), id],
                map_attempt,
            )
            .optional()?;
        let current = current.ok_or_else(|| {
            StorageError::NotFound(format!("remote inference attempt not found: {id}"))
        })?;
        if current.state.is_final() {
            return Err(StorageError::OperationConflict(format!(
                "remote inference attempt {id} is already {}",
                current.state.as_str()
            )));
        }
        tx.execute(
            "UPDATE remote_inference_attempts
             SET state = ?3, outcome = ?4, error_class = ?5, model_used = ?6,
                 usage_source = ?7, tokens_in = ?8, tokens_out = ?9, latency_ms = ?10,
                 updated_at = ?11
             WHERE owner_class = 'user' AND owner_uid = ?1 AND id = ?2",
            params![
                caller.uid(),
                id,
                state.as_str(),
                update.outcome,
                update.error_class,
                update.model_used,
                update.usage_source,
                to_i64(update.tokens_in),
                to_i64(update.tokens_out),
                to_i64(update.latency_ms),
                Timestamp::now().as_millis()
            ],
        )?;
        let updated = tx.query_row(
            &format!("SELECT {ATTEMPT_COLUMNS} FROM remote_inference_attempts WHERE id = ?1"),
            params![id],
            map_attempt,
        )?;
        tx.commit()?;
        Ok(updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Store;

    const ALICE: Caller = Caller::new(1001);
    const BOB: Caller = Caller::new(1002);

    fn store_with_workload() -> (Store, String) {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let workload = format!("wl-{}", uuid::Uuid::new_v4().simple());
        let profile = crate::types::ProfileId::new();
        store
            .create_profile(
                &profile,
                "remote",
                "system",
                "fallback = 'fail'\n",
                "{}",
                crate::types::Owner::System,
                None,
                &[],
            )
            .unwrap();
        store
            .conn
            .execute_batch(&format!(
                "INSERT INTO inference_workloads
                     (id, profile_id, profile_name, model_id, background, priority, state, reason,
                      ceilings, created_by, created_at, owner_class, owner_uid)
                 VALUES ('{workload}', '{profile}', 'p', 'model-x', 'none', 0, 'ADMITTED', 'r',
                         '{{}}', 'uid:1001', 0, 'user', 1001);"
            ))
            .unwrap();
        (store, workload)
    }

    #[test]
    fn consent_is_scoped_bounded_and_revocable_once() {
        let (mut store, _) = store_with_workload();
        assert!(store
            .grant_remote_consent(ALICE, "p1", "h.example", 0)
            .is_err());
        assert!(store
            .grant_remote_consent(ALICE, "p1", "h.example", 31)
            .is_err());
        let grant = store
            .grant_remote_consent(ALICE, "p1", "h.example", 30)
            .unwrap();
        let now = grant.granted_at;
        assert!(store
            .active_remote_consent(ALICE, "p1", "h.example", now)
            .unwrap()
            .is_some());
        for (caller, provider, host) in [
            (BOB, "p1", "h.example"),
            (ALICE, "p2", "h.example"),
            (ALICE, "p1", "other.example"),
        ] {
            assert!(
                store
                    .active_remote_consent(caller, provider, host, now)
                    .unwrap()
                    .is_none(),
                "{caller:?} {provider} {host}"
            );
        }
        // Expiry is exclusive and at most 30 days out.
        assert_eq!(grant.expires_at - grant.granted_at, 30 * DAY_MS);
        assert!(store
            .active_remote_consent(ALICE, "p1", "h.example", grant.expires_at)
            .unwrap()
            .is_none());
        // Bob cannot revoke Alice's grant; Alice can, once.
        assert_eq!(
            store
                .revoke_remote_consent(BOB, Some(&grant.id), None)
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .revoke_remote_consent(ALICE, None, Some("p1"))
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .revoke_remote_consent(ALICE, Some(&grant.id), None)
                .unwrap(),
            0
        );
        assert!(store
            .active_remote_consent(ALICE, "p1", "h.example", now)
            .unwrap()
            .is_none());
        assert!(store.list_remote_consents(BOB).unwrap().is_empty());
        assert!(store.list_remote_consents(ALICE).unwrap()[0]
            .revoked_at
            .is_some());
        // The scope of a grant is immutable even to a raw writer.
        let widened = store.conn.execute(
            "UPDATE remote_inference_consents SET destination = 'evil.example', revoked_at = NULL",
            [],
        );
        assert!(widened.is_err(), "a grant's scope was changed");
        let extended = store.conn.execute(
            "UPDATE remote_inference_consents SET expires_at = expires_at + 1",
            [],
        );
        assert!(extended.is_err(), "a grant was extended");
    }

    #[test]
    fn attempts_are_owner_scoped_unique_per_operation_and_final() {
        let (mut store, workload) = store_with_workload();
        let grant = store
            .grant_remote_consent(ALICE, "p1", "h.example", 1)
            .unwrap();
        let digest = "a".repeat(64);
        let attempt = |id: &'static str, operation: Option<&'static str>| NewAttempt {
            id,
            workload_id: Box::leak(workload.clone().into_boxed_str()),
            provider_id: "p1",
            destination: "h.example",
            model: "m",
            consent_id: Box::leak(grant.id.clone().into_boxed_str()),
            operation_id: operation,
            request_id: id,
            request_digest: Box::leak(digest.clone().into_boxed_str()),
        };
        store
            .insert_remote_attempt(ALICE, &attempt("att-1", Some("op-1")))
            .unwrap();
        assert!(matches!(
            store.insert_remote_attempt(ALICE, &attempt("att-2", Some("op-1"))),
            Err(StorageError::OperationConflict(_))
        ));
        store
            .insert_remote_attempt(ALICE, &attempt("att-3", None))
            .unwrap();
        store
            .insert_remote_attempt(ALICE, &attempt("att-4", None))
            .unwrap();
        assert_eq!(store.count_open_remote_attempts(ALICE, "p1").unwrap(), 3);
        assert!(store.remote_attempt_for(BOB, "att-1").unwrap().is_none());
        assert!(store
            .remote_attempt_by_operation(BOB, "op-1")
            .unwrap()
            .is_none());
        assert!(matches!(
            store.update_remote_attempt(
                BOB,
                "att-1",
                AttemptState::Succeeded,
                &AttemptUpdate::default()
            ),
            Err(StorageError::NotFound(_))
        ));

        let unknown = AttemptUpdate {
            outcome: Some("unknown".into()),
            error_class: Some("timeout".into()),
            ..AttemptUpdate::default()
        };
        let done = store
            .update_remote_attempt(ALICE, "att-1", AttemptState::Unknown, &unknown)
            .unwrap();
        assert_eq!(done.state, AttemptState::Unknown);
        assert_eq!(done.tokens_in, None, "unknown usage must stay NULL");
        // An unknown outcome is final: it cannot become a success.
        assert!(matches!(
            store.update_remote_attempt(
                ALICE,
                "att-1",
                AttemptState::Succeeded,
                &AttemptUpdate::default()
            ),
            Err(StorageError::OperationConflict(_))
        ));
        assert!(store
            .conn
            .execute(
                "UPDATE remote_inference_attempts SET state = 'succeeded' WHERE id = 'att-1'",
                []
            )
            .is_err());
        let ok = AttemptUpdate {
            outcome: Some("sent".into()),
            usage_source: Some("provider_reported".into()),
            tokens_in: Some(12),
            tokens_out: Some(3),
            latency_ms: Some(40),
            model_used: Some("m".into()),
            ..AttemptUpdate::default()
        };
        let succeeded = store
            .update_remote_attempt(ALICE, "att-3", AttemptState::Succeeded, &ok)
            .unwrap();
        assert_eq!(
            (succeeded.tokens_in, succeeded.tokens_out),
            (Some(12), Some(3))
        );
        assert_eq!(store.count_open_remote_attempts(ALICE, "p1").unwrap(), 1);
    }

    /// Owner Q4: the attempt schema has no column that can hold text.
    #[test]
    fn attempts_have_no_prompt_or_response_columns() {
        let (store, _) = store_with_workload();
        let mut statement = store
            .conn
            .prepare("PRAGMA table_info(remote_inference_attempts)")
            .unwrap();
        let columns: Vec<String> = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for forbidden in ["prompt", "text", "response", "content", "body", "message"] {
            assert!(
                columns.iter().all(|column| !column.contains(forbidden)),
                "column containing `{forbidden}` in {columns:?}"
            );
        }
    }
}
