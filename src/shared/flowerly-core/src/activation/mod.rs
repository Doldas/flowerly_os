//! `flowerlyd`'s side of activating a Generation (ADR-0023 section 2, R3, R4;
//! G4b): the release gate, the client of the root slot broker, and the order
//! of the steps.
//!
//! 0. The Generation must exist (an Admin sees every Generation, as for
//!    `promote`; the database refuses a non-system one later).
//! 1. Activation must be enabled: [`RELEASE_ACTIVATION_ENABLED`] is off until
//!    Gate D is verified, so only an image carrying the root-owned test-profile
//!    marker gets past this point. Nothing is written before it.
//! 2. A retry with a known `operation_id` returns the recorded row and asks no
//!    broker (an unknown outcome is read back, never re-sent).
//! 3. The broker's read-only `plan` verifies the staged slot against the
//!    Generation's recorded identity and says which slot and PARTUUID it would
//!    arm and whether the activation is irreversible. The Admin's
//!    acknowledgement must match that answer exactly.
//! 4. Every database guard runs on a rolled-back insert, so a refused request
//!    takes no backup and arms nothing.
//! 5. An irreversible activation takes its coherent backup.
//! 6. The PENDING row is written, with the ledger position read now.
//! 7. Only then the broker is asked to `arm`. A refusal abandons the row with
//!    the broker's reason; a lost answer leaves it PENDING for the witness.
//!
//! The store lock is not held while the broker hashes the slots.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use serde_json::{json, Value};

use crate::storage::lifecycle::{Activation, ActivationKind, ActivationRequest};
use crate::storage::{StorageError, Store};
use crate::types::{Caller, GenerationId};
use crate::witness::{read_ledger, LedgerSource};

/// Off until the change that records the passing G5 evidence (ADR-0023); the
/// broker has its own copy and decides for itself.
pub const RELEASE_ACTIVATION_ENABLED: bool = false;
pub const BROKER_SOCKET: &str = "/run/flowerly-slot-broker.sock";
/// Installed only by the explicit Generation A/B test-image script.
pub const TEST_PROFILE_MARKER: &str = "/usr/lib/flowerly/generation-ab-test-profile";
/// Under the state directory; the broker's config names the same place.
pub const BACKUP_DIR: &str = "activation-backups";
const TRIES: u32 = 3;
const MAX_ANSWER: usize = 64 * 1024;
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// The broker hashes both slots (and a backup) before it answers.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(300);

/// Where activation reaches outside the database.
#[derive(Debug, Clone)]
pub struct ActivationHost {
    pub broker_socket: PathBuf,
    pub test_profile_marker: PathBuf,
    /// Root in production: a marker the service account could create does
    /// not enable anything.
    pub marker_owner_uid: u32,
    pub ledger: LedgerSource,
    pub backup_dir: PathBuf,
}

impl ActivationHost {
    pub fn for_state_dir(state_dir: &Path) -> std::io::Result<Self> {
        Ok(Self {
            broker_socket: PathBuf::from(BROKER_SOCKET),
            test_profile_marker: PathBuf::from(TEST_PROFILE_MARKER),
            marker_owner_uid: 0,
            ledger: LedgerSource::for_state_dir(state_dir)?,
            backup_dir: state_dir.join(BACKUP_DIR),
        })
    }

    /// The release switch, or a test-profile marker that is a regular file
    /// (not a symlink) of the required owner and not writable by others.
    pub fn enabled(&self) -> bool {
        RELEASE_ACTIVATION_ENABLED
            || std::fs::symlink_metadata(&self.test_profile_marker).is_ok_and(|meta| {
                meta.is_file() && meta.uid() == self.marker_owner_uid && meta.mode() & 0o022 == 0
            })
    }
}

/// What the Admin asked for.
#[derive(Debug, Clone)]
pub struct ActivateCommand {
    pub generation: GenerationId,
    pub kind: ActivationKind,
    pub reason: String,
    pub operation_id: Option<String>,
    /// The FELL_BACK activation this request decides to retry despite.
    pub acknowledges: Option<i64>,
    /// The Admin accepts that the fallback becomes manual-only.
    pub acknowledge_irreversible: bool,
}

#[derive(Debug)]
pub enum Outcome {
    /// Recorded PENDING and armed; success still needs the witnessed boot.
    Armed {
        activation: Box<Activation>,
        broker: Value,
    },
    /// The `operation_id` was already used for this request.
    AlreadyRequested(Box<Activation>),
}

#[derive(Debug)]
pub enum Failure {
    Disabled,
    Invalid(String),
    /// Something outside the database (the boot ledger) cannot be trusted.
    Unavailable(String),
    Storage(StorageError),
    /// The broker refused or could not be reached. With a row, it is now
    /// ABANDONED; without one, nothing was written.
    Refused {
        activation: Option<Box<Activation>>,
        reason: String,
    },
    /// The arm was sent but its answer was lost; the row stays PENDING.
    Unknown {
        activation: Box<Activation>,
        reason: String,
    },
}

impl From<StorageError> for Failure {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

#[derive(Debug)]
pub enum BrokerError {
    /// Not connected: nothing was sent.
    Unavailable(String),
    Refused(String),
    /// Sent, but no complete answer came back.
    Unknown(String),
}

/// One intent, one answer line.
pub fn call_broker(socket: &Path, intent: &Value) -> Result<Value, BrokerError> {
    let stream = UnixStream::connect(socket)
        .map_err(|e| BrokerError::Unavailable(format!("the slot broker is not reachable: {e}")))?;
    let unknown = |e: std::io::Error| BrokerError::Unknown(format!("slot broker: {e}"));
    stream
        .set_write_timeout(Some(WRITE_TIMEOUT))
        .map_err(unknown)?;
    stream
        .set_read_timeout(Some(ANSWER_TIMEOUT))
        .map_err(unknown)?;
    let mut line = intent.to_string().into_bytes();
    line.push(b'\n');
    (&stream).write_all(&line).map_err(unknown)?;
    let mut answer = Vec::new();
    BufReader::new((&stream).take(MAX_ANSWER as u64 + 1))
        .read_until(b'\n', &mut answer)
        .map_err(unknown)?;
    if answer.len() > MAX_ANSWER || answer.last() != Some(&b'\n') {
        return Err(BrokerError::Unknown(
            "the slot broker gave no complete answer".into(),
        ));
    }
    let reply: Value = serde_json::from_slice(&answer)
        .map_err(|_| BrokerError::Unknown("the slot broker's answer is not JSON".into()))?;
    if reply["ok"] == true {
        Ok(reply)
    } else {
        Err(BrokerError::Refused(
            reply["reason"]
                .as_str()
                .unwrap_or("the slot broker refused without a reason")
                .to_owned(),
        ))
    }
}

fn lock(store: &Mutex<Store>) -> Result<MutexGuard<'_, Store>, Failure> {
    store
        .lock()
        .map_err(|_| Failure::Invalid("storage lock poisoned".into()))
}

/// Run one Admin activation or rollback request; see the module docs.
/// Without a host (no state directory) activation is disabled.
pub fn activate(
    store: &Mutex<Store>,
    host: Option<&ActivationHost>,
    admin: Caller,
    command: &ActivateCommand,
) -> Result<Outcome, Failure> {
    lock(store)?
        .get_generation(&command.generation)?
        .ok_or_else(|| StorageError::NotFound(format!("generation {}", command.generation)))?;
    let Some(host) = host.filter(|host| host.enabled()) else {
        return Err(Failure::Disabled);
    };
    let claim = {
        let store = lock(store)?;
        if let Some(key) = &command.operation_id {
            if let Some(existing) = store.activation_for_operation(key)? {
                if existing.generation != command.generation
                    || existing.kind != command.kind
                    || existing.actor_uid != admin.uid()
                {
                    return Err(Failure::Storage(StorageError::OperationConflict(
                        "operation_id was already used for a different activation request".into(),
                    )));
                }
                return Ok(Outcome::AlreadyRequested(Box::new(existing)));
            }
        }
        let generation = store
            .get_generation(&command.generation)?
            .ok_or_else(|| StorageError::NotFound(format!("generation {}", command.generation)))?;
        let (Some(source_commit), Some(identity)) = (
            generation.source_commit,
            store.root_identity(&command.generation)?,
        ) else {
            return Err(Failure::Invalid(format!(
                "generation {} has no complete root identity",
                command.generation
            )));
        };
        json!({
            "rootfs_sha256": identity.rootfs_sha256,
            "kernel_sha256": identity.kernel_sha256,
            "initramfs_sha256": identity.initramfs_sha256,
            "source_commit": source_commit,
            "input_manifest_sha256": identity.input_manifest_sha256,
            "build_id": identity.build_id,
        })
    };
    let ledger = read_ledger(&host.ledger.path, &host.ledger.policy)
        .map_err(|e| Failure::Unavailable(format!("the boot ledger cannot be verified: {e}")))?;

    let intent = |kind: &str| {
        json!({
            "version": 1,
            "intent": kind,
            "admin_uid": admin.uid(),
            "generation": claim,
        })
    };
    // The broker derives the inactive slot itself and names it in the plan.
    let plan = call_broker(&host.broker_socket, &intent("plan")).map_err(|error| {
        let (BrokerError::Refused(reason)
        | BrokerError::Unavailable(reason)
        | BrokerError::Unknown(reason)) = error;
        Failure::Refused {
            activation: None,
            reason,
        }
    })?;
    if plan["status"] != "PLANNED" {
        return Err(Failure::Invalid(
            "the slot broker answered the plan with something else".into(),
        ));
    }
    let slot = plan["target"].as_str().unwrap_or_default().to_owned();
    let partuuid = plan["root_partuuid"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let Some(irreversible) = plan["irreversible"].as_bool() else {
        return Err(Failure::Invalid(
            "the slot broker's plan has no irreversible verdict".into(),
        ));
    };
    match (irreversible, command.acknowledge_irreversible) {
        (true, false) => {
            return Err(Failure::Invalid(format!(
                "irreversible activation: the target needs min_reader {}, the running root knows schema {}; the fallback would be manual-only, so it needs the Admin's explicit acknowledgement",
                plan["target_min_reader"], plan["fallback_schema_version"]
            )))
        }
        (false, true) => {
            return Err(Failure::Invalid(
                "the activation is reversible; an irreversible acknowledgement is refused so the record stays true".into(),
            ))
        }
        _ => {}
    }
    let mut request = ActivationRequest {
        generation: command.generation.clone(),
        kind: command.kind,
        reason: command.reason.clone(),
        operation_id: command.operation_id.clone(),
        expected_slot: slot.clone(),
        expected_root_partuuid: partuuid,
        ledger_sequence: ledger.last_sequence(),
        ledger_head: ledger.head().to_owned(),
        irreversible,
        // Checked shape only; the real hash replaces it before the insert.
        backup_sha256: irreversible.then(ActivationRequest::empty_ledger_head),
        acknowledges: command.acknowledges,
    };
    let pending = {
        let mut store = lock(store)?;
        store.check_activation_request(admin, &request)?;
        if irreversible {
            request.backup_sha256 = Some(store.backup_into(&host.backup_dir)?.sha256);
        }
        store.request_activation(admin, &request)?
    };

    let mut arm = intent("arm");
    arm["slot"] = json!(slot);
    arm["tries"] = json!(TRIES);
    if let Some(digest) = &request.backup_sha256 {
        arm["irreversible"] = json!({ "backup_sha256": digest });
    }
    match call_broker(&host.broker_socket, &arm) {
        Ok(broker) => Ok(Outcome::Armed {
            activation: Box::new(pending),
            broker,
        }),
        Err(BrokerError::Refused(reason) | BrokerError::Unavailable(reason)) => {
            let abandoned = lock(store)?
                .abandon_activation(pending.id, &format!("the slot broker refused: {reason}"))?;
            Err(Failure::Refused {
                activation: Some(Box::new(abandoned)),
                reason,
            })
        }
        Err(BrokerError::Unknown(reason)) => Err(Failure::Unknown {
            activation: Box::new(pending),
            reason,
        }),
    }
}

#[cfg(test)]
mod tests;
