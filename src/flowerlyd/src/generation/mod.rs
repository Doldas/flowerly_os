use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Context;
use flowerly_core::ipc::{Envelope, ErrorCode, Payload};
use flowerly_core::storage::lifecycle::{
    validate_operation_id, Activation, EvidenceOutcome, Resolution, RootIdentity,
};
use flowerly_core::storage::{StorageError, Store, MAX_AUDIT_REASON};
use flowerly_core::types::{Caller, Owner};
use flowerly_core::types::{
    ExperimentId, Generation, GenerationId, GenerationStatus, GoalId, ObjectRef, ProvenanceEdge,
    ProvenanceRelation, Timestamp,
};
use flowerly_core::witness::{resolve_from, LedgerSource};
use flowerly_core::{coded_error_envelope, error_envelope, ok_envelope};
use serde::Deserialize;
use serde_json::Value;

pub(crate) fn handle_command(
    store: &Arc<Mutex<Store>>,
    request: &Envelope,
    caller: Caller,
) -> Envelope {
    let Payload::Command { name, args } = &request.payload else {
        return error_envelope(request.id.clone(), "unexpected request type");
    };
    let mut store = match store.lock() {
        Ok(store) => store,
        Err(_) => return error_envelope(request.id.clone(), "storage lock poisoned"),
    };
    if let Err((code, message)) = validate_generation_command(&store, name, args, caller) {
        return coded_error_envelope(request.id.clone(), code, message);
    }
    match run_command(&mut store, name, args, caller) {
        Ok(data) => ok_envelope(request.id.clone(), data),
        Err(error) => coded_error_envelope(request.id.clone(), error.code, error.message),
    }
}

/// Typed IPC-v1 argument validation. Unknown fields are tolerated to preserve
/// compatibility with older/newer clients; malformed known fields fail closed.
#[derive(Debug, Deserialize)]
struct CreateGenerationArgs {
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    reason_goal: Option<String>,
    #[serde(default)]
    experiment: Option<String>,
    #[serde(default)]
    source_commit: Option<String>,
    #[serde(default)]
    system_manifest: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GenerationIdArgs {
    id: String,
}

#[derive(Debug, Deserialize)]
struct GenerationListArgs {}

#[derive(Debug, Deserialize)]
struct GenerationPromoteArgs {
    id: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
struct GenerationReasonArgs {
    id: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
struct GenerationIdentityArgs {
    id: String,
    source_commit: String,
    rootfs_sha256: String,
    kernel_sha256: String,
    initramfs_sha256: String,
    input_manifest_sha256: String,
    build_id: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
struct GenerationEvidenceArgs {
    id: String,
    outcome: String,
    evidence_sha256: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
struct GenerationTestArgs {
    id: String,
    experiment: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
struct GenerationActivationArgs {
    id: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    operation_id: Option<String>,
    #[serde(default)]
    acknowledges: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct GenerationResolveArgs {}

#[derive(Debug, Deserialize)]
struct GenerationHistoryArgs {
    #[serde(default)]
    id: Option<String>,
}

/// Longest `source_commit` or `system_manifest` accepted on create.
const MAX_GENERATION_TEXT: usize = 256;

/// Optional free-form fields stay accepted (Q6) but are validated, not
/// trusted: a complete typed identity is what `TESTED` requires.
fn validate_generation_text(field: &str, value: Option<&str>) -> Result<(), (ErrorCode, String)> {
    match value {
        Some(text)
            if text.is_empty()
                || text.len() > MAX_GENERATION_TEXT
                || !text.bytes().all(|b| b.is_ascii_graphic()) =>
        {
            Err((
                ErrorCode::InvalidArgument,
                format!("`{field}` must be 1..{MAX_GENERATION_TEXT} visible ASCII characters"),
            ))
        }
        _ => Ok(()),
    }
}

fn validate_reason_arg(reason: &str) -> Result<(), (ErrorCode, String)> {
    if reason.trim().is_empty() || reason.len() > MAX_AUDIT_REASON {
        return Err((
            ErrorCode::InvalidArgument,
            format!("`reason` must be 1..{MAX_AUDIT_REASON} bytes and not blank"),
        ));
    }
    Ok(())
}

fn validate_generation_id(raw: &str) -> Result<GenerationId, (ErrorCode, String)> {
    GenerationId::from_str(raw).map_err(|error| (ErrorCode::InvalidArgument, error.to_string()))
}

fn typed_generation_args<T: serde::de::DeserializeOwned>(
    args: &Value,
) -> Result<T, (ErrorCode, String)> {
    crate::args::typed_args("generation", args)
}

/// Validate client-facing generation errors while the store lock is held.
/// SQLite errors remain internal instead of being inferred from error text.
fn validate_generation_command(
    store: &Store,
    name: &str,
    args: &Value,
    caller: Caller,
) -> Result<(), (ErrorCode, String)> {
    match name {
        "generation.create" => {
            let typed: CreateGenerationArgs = typed_generation_args(args)?;
            let invalid = |error: String| (ErrorCode::InvalidArgument, error);
            let parent: Option<GenerationId> = typed
                .parent
                .map(|raw| GenerationId::from_str(&raw).map_err(|error| invalid(error.to_string())))
                .transpose()?;
            let reason_goal: Option<GoalId> = typed
                .reason_goal
                .map(|raw| GoalId::from_str(&raw).map_err(|error| invalid(error.to_string())))
                .transpose()?;
            let _: Option<ExperimentId> = typed
                .experiment
                .map(|raw| ExperimentId::from_str(&raw).map_err(|error| invalid(error.to_string())))
                .transpose()?;
            validate_generation_text("source_commit", typed.source_commit.as_deref())?;
            validate_generation_text("system_manifest", typed.system_manifest.as_deref())?;
            // Only a parent the caller can see (own or system) (#225 PR 6).
            if let Some(parent) = &parent {
                if store
                    .generation_for(caller, parent)
                    .map_err(|e| (storage_error_code(&e), e.to_string()))?
                    .is_none()
                {
                    return Err((
                        ErrorCode::NotFound,
                        format!("generation not found: {parent}"),
                    ));
                }
            }
            // Only the caller's own Goal; another uid's is `not_found` (#225).
            if let Some(goal_id) = &reason_goal {
                if store
                    .goal_for(caller, goal_id)
                    .map_err(|e| (storage_error_code(&e), e.to_string()))?
                    .is_none()
                {
                    return Err((ErrorCode::NotFound, format!("goal not found: {goal_id}")));
                }
            }
        }
        "generation.list" => {
            let _: GenerationListArgs = typed_generation_args(args)?;
        }
        "generation.inspect" => {
            let typed: GenerationIdArgs = typed_generation_args(args)?;
            let id = GenerationId::from_str(&typed.id)
                .map_err(|error| (ErrorCode::InvalidArgument, error.to_string()))?;
            if store
                .generation_for(caller, &id)
                .map_err(|e| (storage_error_code(&e), e.to_string()))?
                .is_none()
            {
                return Err((ErrorCode::NotFound, format!("generation not found: {id}")));
            }
        }
        // Admin: existence and ownership are checked in the transaction.
        "generation.activate" | "generation.rollback" => {
            let typed: GenerationActivationArgs = typed_generation_args(args)?;
            validate_generation_id(&typed.id)?;
            if let Some(reason) = &typed.reason {
                validate_reason_arg(reason)?;
            }
            if let Some(key) = &typed.operation_id {
                validate_operation_id(key)
                    .map_err(|error| (ErrorCode::InvalidArgument, error.to_string()))?;
            }
            let _ = typed.acknowledges;
        }
        "generation.identity" => {
            let typed: GenerationIdentityArgs = typed_generation_args(args)?;
            validate_generation_id(&typed.id)?;
            validate_reason_arg(&typed.reason)?;
        }
        "generation.evidence" => {
            let typed: GenerationEvidenceArgs = typed_generation_args(args)?;
            validate_generation_id(&typed.id)?;
            validate_reason_arg(&typed.reason)?;
            EvidenceOutcome::from_str(&typed.outcome)
                .map_err(|error| (ErrorCode::InvalidArgument, error))?;
        }
        "generation.test" => {
            let typed: GenerationTestArgs = typed_generation_args(args)?;
            validate_generation_id(&typed.id)?;
            validate_reason_arg(&typed.reason)?;
            ExperimentId::from_str(&typed.experiment)
                .map_err(|error| (ErrorCode::InvalidArgument, error.to_string()))?;
        }
        "generation.reject" | "generation.revoke" => {
            let typed: GenerationReasonArgs = typed_generation_args(args)?;
            validate_generation_id(&typed.id)?;
            validate_reason_arg(&typed.reason)?;
        }
        "generation.history" => {
            let typed: GenerationHistoryArgs = typed_generation_args(args)?;
            if let Some(id) = &typed.id {
                validate_generation_id(id)?;
            }
        }
        "generation.resolve" => {
            let _: GenerationResolveArgs = typed_generation_args(args)?;
        }
        "generation.promote" => {
            let typed: GenerationPromoteArgs = typed_generation_args(args)?;
            validate_generation_id(&typed.id)?;
            validate_reason_arg(&typed.reason)?;
        }
        other => {
            return Err((
                ErrorCode::InvalidArgument,
                format!("generationd: unknown command `{other}`"),
            ));
        }
    }
    Ok(())
}

fn storage_error_code(error: &StorageError) -> ErrorCode {
    error.ipc_error_code()
}

#[derive(Debug)]
struct CommandFailure {
    code: ErrorCode,
    message: String,
}

impl CommandFailure {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::InvalidArgument,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::NotFound,
            message: message.into(),
        }
    }

    fn storage(error: StorageError) -> Self {
        Self {
            code: storage_error_code(&error),
            message: error.to_string(),
        }
    }
}

impl From<String> for CommandFailure {
    fn from(message: String) -> Self {
        Self {
            code: ErrorCode::Internal,
            message,
        }
    }
}

impl From<&str> for CommandFailure {
    fn from(message: &str) -> Self {
        Self::from(message.to_owned())
    }
}

impl std::fmt::Display for CommandFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[cfg(test)]
impl CommandFailure {
    fn contains(&self, pattern: &str) -> bool {
        self.message.contains(pattern)
    }
}

impl PartialEq<String> for CommandFailure {
    fn eq(&self, other: &String) -> bool {
        &self.message == other
    }
}

static LEDGER: OnceLock<LedgerSource> = OnceLock::new();

/// The boot ledger next to the state database (ADR-0023 G3).
pub(crate) fn configure_ledger(state_dir: &Path) -> anyhow::Result<()> {
    let source = LedgerSource::for_state_dir(state_dir)
        .with_context(|| format!("reading {}", state_dir.display()))?;
    LEDGER
        .set(source)
        .map_err(|_| anyhow::anyhow!("boot ledger already configured"))
}

fn resolve_with(store: &mut Store, source: &LedgerSource) -> Result<Value, CommandFailure> {
    let resolution = resolve_from(store, source).map_err(CommandFailure::storage)?;
    Ok(resolution_json(resolution))
}

/// The IPC answer for a resolution; a request that stays unresolved is logged.
fn resolution_json(resolution: Resolution) -> Value {
    match resolution {
        Resolution::NoPending => serde_json::json!({ "outcome": "none" }),
        Resolution::Waiting { activation, reason } => serde_json::json!({
            "outcome": "waiting", "activation": activation, "reason": reason,
        }),
        Resolution::Unresolved { activation, reason } => {
            tracing::warn!(activation, %reason, "pending Generation activation stays unresolved");
            serde_json::json!({ "outcome": "unresolved", "activation": activation, "reason": reason })
        }
        Resolution::Resolved(activation) => {
            let outcome = match activation.state.as_str() {
                "CONFIRMED" => "confirmed",
                "FELL_BACK" => "fell_back",
                _ => "abandoned",
            };
            tracing::info!(
                activation = activation.id,
                generation_id = %activation.generation,
                outcome,
                "Generation activation resolved from the boot ledger"
            );
            serde_json::json!({ "outcome": outcome, "activation": activation_json(&activation) })
        }
    }
}

/// Start-up resolution; never stops the daemon on the known-good root.
pub(crate) fn resolve_at_startup(store: &mut Store) {
    let Some(source) = LEDGER.get() else {
        return;
    };
    match resolve_with(store, source) {
        Ok(outcome) => tracing::info!(%outcome, "Generation activation check at start-up"),
        Err(error) => tracing::warn!(%error, "Generation activation check failed at start-up"),
    }
}

fn parsed_args<T: serde::de::DeserializeOwned>(args: &Value) -> Result<T, CommandFailure> {
    serde_json::from_value(args.clone()).map_err(|error| CommandFailure::invalid(error.to_string()))
}

fn parsed_id(raw: &str) -> Result<GenerationId, CommandFailure> {
    GenerationId::from_str(raw).map_err(|error| CommandFailure::invalid(error.to_string()))
}

/// Admin only, so the actor and reason are shown as recorded.
fn activation_json(activation: &Activation) -> Value {
    serde_json::json!({
        "id": activation.id,
        "generation": activation.generation,
        "previous_generation": activation.previous_generation,
        "kind": activation.kind.as_str(),
        "state": activation.state.as_str(),
        "actor_uid": activation.actor_uid,
        "reason": activation.reason,
        "operation_id": activation.operation_id,
        "expected_slot": activation.expected_slot,
        "expected_root_partuuid": activation.expected_root_partuuid,
        "ledger_sequence": activation.ledger_sequence,
        "ledger_head": activation.ledger_head,
        "irreversible": activation.irreversible,
        "backup_sha256": activation.backup_sha256,
        "acknowledges": activation.acknowledges,
        "requested_at": activation.requested_at,
        "resolved_at": activation.resolved_at,
        "witness_boot_id": activation.witness_boot_id,
        "witness_sequence": activation.witness_sequence,
        "detail": activation.detail,
    })
}

fn run_command(
    store: &mut Store,
    name: &str,
    args: &Value,
    caller: Caller,
) -> Result<Value, CommandFailure> {
    match name {
        "generation.create" => {
            let parent: Option<GenerationId> = parse_optional_typed(args, "parent")?;
            // Rechecked in the inserting transaction and by a 0013 trigger.
            if let Some(parent) = &parent {
                if store
                    .generation_for(caller, parent)
                    .map_err(CommandFailure::storage)?
                    .is_none()
                {
                    return Err(CommandFailure::not_found(format!(
                        "generation not found: {parent}"
                    )));
                }
            }

            let reason_goal: Option<GoalId> = parse_optional_typed(args, "reason_goal")?;
            // Rechecked in the inserting transaction and by a 0011 trigger.
            if let Some(goal_id) = &reason_goal {
                if store
                    .goal_for(caller, goal_id)
                    .map_err(CommandFailure::storage)?
                    .is_none()
                {
                    return Err(CommandFailure::not_found(format!(
                        "goal not found: {goal_id}"
                    )));
                }
            }

            let experiment: Option<ExperimentId> = parse_optional_typed(args, "experiment")?;
            let generation = Generation {
                id: GenerationId::new(),
                parent,
                source_commit: parse_optional_string(args, "source_commit")?,
                system_manifest: parse_optional_string(args, "system_manifest")?,
                reason_goal,
                experiment,
                status: GenerationStatus::Candidate,
                created_at: Timestamp::now(),
                // Candidates are private to their creator (#225); only an
                // audited Admin promotion makes a shared system record.
                owner: caller.owner(),
            };

            let mut edges = Vec::new();
            if let Some(parent) = &generation.parent {
                edges.push(ProvenanceEdge {
                    from: ObjectRef::from(&generation.id),
                    relation: ProvenanceRelation::CausedBy,
                    to: ObjectRef::from(parent),
                    created_at: generation.created_at,
                    note: Some("parent generation".to_owned()),
                });
            }
            if let Some(goal_id) = &generation.reason_goal {
                edges.push(ProvenanceEdge {
                    from: ObjectRef::from(&generation.id),
                    relation: ProvenanceRelation::CausedBy,
                    to: ObjectRef::from(goal_id),
                    created_at: generation.created_at,
                    note: Some("reason goal".to_owned()),
                });
            }
            if let Some(experiment) = &generation.experiment {
                edges.push(ProvenanceEdge {
                    from: ObjectRef::from(&generation.id),
                    relation: ProvenanceRelation::TestedBy,
                    to: ObjectRef::from(experiment),
                    created_at: generation.created_at,
                    note: None,
                });
            }

            store
                .insert_generation_with_provenance(&generation, &edges)
                .map_err(CommandFailure::storage)?;
            tracing::info!(
                generation_id = %generation.id,
                provenance_edges = edges.len(),
                "generation created"
            );
            Ok(serde_json::json!({
                "id": generation.id,
                "status": generation.status,
                "parent": generation.parent,
                "source_commit": generation.source_commit,
                "reason_goal": generation.reason_goal,
                "experiment": generation.experiment,
            }))
        }
        "generation.list" => {
            let items: Vec<Value> = store
                .generations_for(caller)
                .map_err(CommandFailure::storage)?
                .iter()
                .map(|g| {
                    serde_json::json!({
                        "id": g.id,
                        "status": g.status,
                        "parent": g.parent,
                        "source_commit": g.source_commit,
                    })
                })
                .collect();
            Ok(serde_json::json!({ "generations": items, "count": items.len() }))
        }
        "generation.inspect" => {
            let id = args
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| CommandFailure::invalid("missing string arg `id`"))?;
            let mut generation = store
                .generation_for(
                    caller,
                    &GenerationId::from_str(id)
                        .map_err(|e| CommandFailure::invalid(e.to_string()))?,
                )
                .map_err(CommandFailure::storage)?
                .ok_or_else(|| CommandFailure::not_found(format!("generation not found: {id}")))?;
            // Visible: own and system Generations (#225 PR 6). A reason Goal
            // the caller cannot see is not revealed, also on a promoted
            // system Generation (redaction contract, PR 4).
            if let Some(goal) = &generation.reason_goal {
                if store
                    .goal_for(caller, goal)
                    .map_err(CommandFailure::storage)?
                    .is_none()
                {
                    generation.reason_goal = None;
                }
            }
            Ok(serde_json::to_value(&generation).map_err(err_str)?)
        }
        "generation.activate" | "generation.rollback" => {
            let typed: GenerationActivationArgs = parsed_args(args)?;
            let id = typed.id.as_str();
            let generation_id = parsed_id(id)?;
            store
                .get_generation(&generation_id)
                .map_err(CommandFailure::storage)?
                .ok_or_else(|| CommandFailure::not_found(format!("generation not found: {id}")))?;
            // STABLE now follows a witnessed boot; no request can set it, and
            // activation stays off until Gate D is verified (ADR-0023).
            Err(CommandFailure::storage(StorageError::OperationConflict(
                format!("generation {id}: activation and rollback are disabled until Gate D is verified"),
            )))
        }
        // Admin, audited (ADR-0023): every actor below is the kernel-attested
        // caller; nothing in the arguments can name one.
        "generation.identity" => {
            let typed: GenerationIdentityArgs = parsed_args(args)?;
            let id = parsed_id(&typed.id)?;
            let identity = RootIdentity {
                rootfs_sha256: typed.rootfs_sha256,
                kernel_sha256: typed.kernel_sha256,
                initramfs_sha256: typed.initramfs_sha256,
                input_manifest_sha256: typed.input_manifest_sha256,
                build_id: typed.build_id,
            };
            store
                .set_root_identity(caller, &id, &typed.source_commit, &identity, &typed.reason)
                .map_err(CommandFailure::storage)?;
            Ok(serde_json::json!({ "id": id }))
        }
        "generation.evidence" => {
            let typed: GenerationEvidenceArgs = parsed_args(args)?;
            let id = parsed_id(&typed.id)?;
            let outcome =
                EvidenceOutcome::from_str(&typed.outcome).map_err(CommandFailure::invalid)?;
            let experiment = store
                .record_evidence(caller, &id, outcome, &typed.evidence_sha256, &typed.reason)
                .map_err(CommandFailure::storage)?;
            Ok(serde_json::json!({ "id": id, "experiment": experiment }))
        }
        "generation.test" => {
            let typed: GenerationTestArgs = parsed_args(args)?;
            let id = parsed_id(&typed.id)?;
            let experiment = ExperimentId::from_str(&typed.experiment)
                .map_err(|error| CommandFailure::invalid(error.to_string()))?;
            let tested = store
                .declare_tested(caller, &id, &ObjectRef::from(&experiment), &typed.reason)
                .map_err(CommandFailure::storage)?;
            Ok(serde_json::json!({ "id": tested.id, "status": tested.status }))
        }
        "generation.reject" | "generation.revoke" => {
            let typed: GenerationReasonArgs = parsed_args(args)?;
            let id = parsed_id(&typed.id)?;
            let finished = if name == "generation.reject" {
                store.reject_generation(caller, &id, &typed.reason)
            } else {
                store.revoke_generation(caller, &id, &typed.reason)
            }
            .map_err(CommandFailure::storage)?;
            Ok(serde_json::json!({ "id": finished.id, "status": finished.status }))
        }
        // Service account only: reads the ledger itself and takes no input.
        "generation.resolve" => {
            let _: GenerationResolveArgs = parsed_args(args)?;
            let source = LEDGER.get().ok_or_else(|| CommandFailure {
                code: ErrorCode::Unavailable,
                message: "no boot ledger is configured".to_owned(),
            })?;
            resolve_with(store, source)
        }
        "generation.history" => {
            let typed: GenerationHistoryArgs = parsed_args(args)?;
            let filter = typed.id.as_deref().map(parsed_id).transpose()?;
            if let Some(id) = &filter {
                store
                    .get_generation(id)
                    .map_err(CommandFailure::storage)?
                    .ok_or_else(|| {
                        CommandFailure::not_found(format!("generation not found: {id}"))
                    })?;
            }
            let items: Vec<Value> = store
                .activation_history()
                .map_err(CommandFailure::storage)?
                .iter()
                .filter(|activation| {
                    filter
                        .as_ref()
                        .is_none_or(|id| activation.generation == *id)
                })
                .map(activation_json)
                .collect();
            Ok(serde_json::json!({ "count": items.len(), "activations": items }))
        }
        // Admin, audited: the caller is the real admin uid (#225 PR 6).
        "generation.promote" => {
            let typed: GenerationPromoteArgs =
                serde_json::from_value(args.clone()).map_err(err_str)?;
            let generation_id = GenerationId::from_str(&typed.id)
                .map_err(|error| CommandFailure::invalid(error.to_string()))?;
            let promoted = store
                .promote_generation(caller, &generation_id, &typed.reason)
                .map_err(CommandFailure::storage)?;
            tracing::info!(
                generation_id = %promoted.id,
                admin_uid = caller.uid(),
                "generation promoted to system"
            );
            Ok(serde_json::json!({
                "id": promoted.id,
                "status": promoted.status,
                "owner": promoted.owner.class(),
            }))
        }
        other => Err(CommandFailure::invalid(format!(
            "generationd: unknown command `{other}`"
        ))),
    }
}

fn parse_optional_typed<T>(args: &Value, key: &str) -> Result<Option<T>, String>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }

    let raw = value
        .as_str()
        .ok_or_else(|| format!("arg `{key}` must be a string or null"))?;
    raw.parse::<T>().map(Some).map_err(err_str)
}

fn parse_optional_string(args: &Value, key: &str) -> Result<Option<String>, String> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }

    value
        .as_str()
        .map(|raw| Some(raw.to_owned()))
        .ok_or_else(|| format!("arg `{key}` must be a string or null"))
}

fn err_str(error: impl std::fmt::Display) -> String {
    error.to_string()
}

pub(crate) async fn self_test() -> anyhow::Result<()> {
    let dir = tempfile::tempdir().context("creating temp dir")?;
    let db = dir.path().join("generationd-self-test.db");
    let mut store = Store::open(&db).context("opening self-test db")?;
    let applied = store.migrate()?;
    let first = Generation {
        id: GenerationId::new(),
        parent: None,
        source_commit: Some("abc123".to_owned()),
        system_manifest: Some("sha256:deadbeef".to_owned()),
        reason_goal: None,
        experiment: None,
        status: GenerationStatus::Candidate,
        created_at: Timestamp::now(),
        owner: Owner::System,
    };
    store.insert_generation(&first)?;
    let second = Generation {
        id: GenerationId::new(),
        parent: Some(first.id.clone()),
        source_commit: Some("def456".to_owned()),
        system_manifest: None,
        reason_goal: None,
        experiment: None,
        status: GenerationStatus::Candidate,
        created_at: Timestamp::now(),
        owner: Owner::System,
    };
    store.insert_generation(&second)?;
    let admin = Caller::new(0);
    store.reject_generation(admin, &first.id, "self-test")?;
    anyhow::ensure!(
        store
            .revoke_generation(admin, &second.id, "self-test")
            .is_err(),
        "a CANDIDATE must not be revocable"
    );
    assert_eq!(store.list_generations()?.len(), 2);
    println!("generationd self-test ok: migrations={}", applied.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CALLER: Caller = Caller::new(1000);
    use flowerly_core::storage::lifecycle::{ActivationKind, ActivationRequest};
    use flowerly_core::types::{Goal, ProvenanceRelation};

    #[test]
    fn activation_malformed_id_is_invalid_argument_not_internal() {
        let mut store = migrated_store();
        let error = run_command(
            &mut store,
            "generation.activate",
            &serde_json::json!({ "id": "malformed-generation-id" }),
            TEST_CALLER,
        )
        .expect_err("malformed activation ID must be rejected");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(store
            .list_generations()
            .expect("list generations")
            .is_empty());
    }

    #[test]
    fn generation_storage_errors_preserve_extended_sqlite_codes() {
        let constraint = |code| {
            StorageError::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(code),
                None,
            ))
        };
        assert_eq!(
            storage_error_code(&constraint(rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE)),
            ErrorCode::Conflict
        );
        assert_eq!(
            storage_error_code(&constraint(rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY)),
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            storage_error_code(&constraint(rusqlite::ffi::SQLITE_CONSTRAINT_CHECK)),
            ErrorCode::Internal
        );
        assert_eq!(
            storage_error_code(&constraint(rusqlite::ffi::SQLITE_CORRUPT)),
            ErrorCode::Internal
        );
        assert_eq!(
            storage_error_code(&constraint(rusqlite::ffi::SQLITE_BUSY)),
            ErrorCode::Unavailable
        );
    }

    #[test]
    fn typed_generation_v1_arguments_accept_legacy_unknown_fields() {
        let args = serde_json::json!({
            "parent": null,
            "reason_goal": null,
            "experiment": null,
            "source_commit": "abc",
            "system_manifest": null,
            "future_optional_field": true
        });
        let parsed: CreateGenerationArgs =
            typed_generation_args(&args).expect("legacy compatible fields");
        assert!(parsed.parent.is_none());
        assert!(parsed.reason_goal.is_none());
        assert_eq!(parsed.source_commit.as_deref(), Some("abc"));
        let _: GenerationListArgs = typed_generation_args(&serde_json::json!({"future": 1}))
            .expect("list ignores unknown fields");
    }

    #[test]
    fn typed_generation_v1_arguments_reject_malformed_fields() {
        for args in [
            serde_json::json!({"parent": 123}),
            serde_json::json!({"reason_goal": false}),
            serde_json::json!({"source_commit": []}),
            serde_json::json!([]),
        ] {
            let error = typed_generation_args::<CreateGenerationArgs>(&args)
                .expect_err("invalid arguments must fail");
            assert_eq!(error.0, ErrorCode::InvalidArgument);
        }
        for args in [
            serde_json::json!({}),
            serde_json::json!({"id": null}),
            serde_json::json!({"id": 123}),
        ] {
            let error = typed_generation_args::<GenerationIdArgs>(&args)
                .expect_err("invalid identity must fail");
            assert_eq!(error.0, ErrorCode::InvalidArgument);
        }
        let store = migrated_store();
        for (name, args) in [
            (
                "generation.create",
                serde_json::json!({"parent": "malformed-id"}),
            ),
            (
                "generation.create",
                serde_json::json!({"experiment": "invalid-id"}),
            ),
            (
                "generation.activate",
                serde_json::json!({"id": "invalid-id"}),
            ),
        ] {
            let error = validate_generation_command(&store, name, &args, TEST_CALLER)
                .expect_err("invalid typed identity must fail");
            assert_eq!(error.0, ErrorCode::InvalidArgument);
        }
    }

    fn migrated_store() -> Store {
        let mut store = Store::open_in_memory().expect("open store");
        store.migrate().expect("migrate store");
        store
    }

    fn candidate(parent: Option<GenerationId>) -> Generation {
        Generation {
            id: GenerationId::new(),
            parent,
            source_commit: Some("base".to_owned()),
            system_manifest: None,
            reason_goal: None,
            experiment: None,
            status: GenerationStatus::Candidate,
            created_at: Timestamp::now(),
            owner: Owner::User(1000),
        }
    }

    #[test]
    fn malformed_generation_references_are_rejected() {
        let mut store = migrated_store();

        let error = run_command(
            &mut store,
            "generation.create",
            &serde_json::json!({ "parent": "not-a-generation" }),
            TEST_CALLER,
        )
        .expect_err("invalid parent must fail");
        assert!(error.contains("generation"), "unexpected error: {error}");

        let error = run_command(
            &mut store,
            "generation.create",
            &serde_json::json!({ "experiment": "not-an-experiment" }),
            TEST_CALLER,
        )
        .expect_err("invalid experiment must fail");
        assert!(error.contains("experiment"), "unexpected error: {error}");

        assert!(store
            .list_generations()
            .expect("list generations")
            .is_empty());
    }

    #[test]
    fn generation_requires_existing_parent_and_reason_goal() {
        let mut store = migrated_store();
        let missing_parent = GenerationId::new();

        let error = run_command(
            &mut store,
            "generation.create",
            &serde_json::json!({ "parent": missing_parent }),
            TEST_CALLER,
        )
        .expect_err("missing parent must fail");
        assert_eq!(error, format!("generation not found: {missing_parent}"));

        let missing_goal = GoalId::new();
        let error = run_command(
            &mut store,
            "generation.create",
            &serde_json::json!({ "reason_goal": missing_goal }),
            TEST_CALLER,
        )
        .expect_err("missing reason goal must fail");
        assert_eq!(error, format!("goal not found: {missing_goal}"));

        assert!(store
            .list_generations()
            .expect("list generations")
            .is_empty());
    }

    #[test]
    fn reason_goal_must_be_the_callers_own_and_is_not_revealed_otherwise() {
        let mut store = migrated_store();
        let mut theirs = Goal::new("theirs", "uid:2000");
        theirs.owner = Caller::new(2000).owner();
        store.insert_goal(&theirs).expect("insert goal");
        let args = serde_json::json!({ "reason_goal": theirs.id });
        let (code, message) =
            validate_generation_command(&store, "generation.create", &args, TEST_CALLER)
                .unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
        assert_eq!(message, format!("goal not found: {}", theirs.id));
        let failure = run_command(&mut store, "generation.create", &args, TEST_CALLER).unwrap_err();
        assert_eq!(failure.code, ErrorCode::NotFound);
        assert!(store.list_generations().unwrap().is_empty());

        // Another uid's candidate is private (#225 PR 6); once an admin
        // promotes it, its reason Goal is still shown only to the Goal's owner.
        let mut legacy = candidate(None);
        legacy.reason_goal = Some(theirs.id.clone());
        legacy.owner = theirs.owner;
        store.insert_generation(&legacy).expect("insert generation");
        let hidden = run_command(
            &mut store,
            "generation.inspect",
            &serde_json::json!({ "id": legacy.id }),
            TEST_CALLER,
        )
        .unwrap_err();
        assert_eq!(hidden.code, ErrorCode::NotFound);
        run_command(
            &mut store,
            "generation.promote",
            &serde_json::json!({ "id": legacy.id, "reason": "share it" }),
            Caller::new(0),
        )
        .unwrap();
        let shown = run_command(
            &mut store,
            "generation.inspect",
            &serde_json::json!({ "id": legacy.id }),
            TEST_CALLER,
        )
        .unwrap();
        assert_eq!(shown["reason_goal"], Value::Null);
        let owner = Caller::new(2000);
        let shown = run_command(
            &mut store,
            "generation.inspect",
            &serde_json::json!({ "id": legacy.id }),
            owner,
        )
        .unwrap();
        assert_eq!(shown["reason_goal"], serde_json::json!(theirs.id));
    }

    #[test]
    fn generation_creation_records_causal_provenance() {
        let mut store = migrated_store();
        let parent = candidate(None);
        store.insert_generation(&parent).expect("insert parent");
        let mut goal = Goal::new("Improve the system", "test");
        goal.owner = TEST_CALLER.owner();
        store.insert_goal(&goal).expect("insert goal");
        let experiment = ExperimentId::new();

        let response = run_command(
            &mut store,
            "generation.create",
            &serde_json::json!({
                "parent": parent.id,
                "reason_goal": goal.id,
                "experiment": experiment,
                "source_commit": "abc123",
                "system_manifest": "sha256:manifest"
            }),
            TEST_CALLER,
        )
        .expect("create generation");

        let id: GenerationId = response["id"]
            .as_str()
            .expect("generation id")
            .parse()
            .expect("typed generation id");
        let generation = store
            .get_generation(&id)
            .expect("generation query")
            .expect("generation exists");
        assert_eq!(generation.parent.as_ref(), Some(&parent.id));
        assert_eq!(generation.reason_goal.as_ref(), Some(&goal.id));
        assert_eq!(generation.experiment.as_ref(), Some(&experiment));

        let edges = store
            .provenance_from(&ObjectRef::from(&id))
            .expect("generation provenance");
        assert_eq!(edges.len(), 3);
        assert!(edges.iter().any(|edge| {
            edge.relation == ProvenanceRelation::CausedBy && edge.to == ObjectRef::from(&parent.id)
        }));
        assert!(edges.iter().any(|edge| {
            edge.relation == ProvenanceRelation::CausedBy && edge.to == ObjectRef::from(&goal.id)
        }));
        assert!(edges.iter().any(|edge| {
            edge.relation == ProvenanceRelation::TestedBy && edge.to == ObjectRef::from(&experiment)
        }));
    }

    #[test]
    fn generation_errors_have_explicit_codes() {
        let store = migrated_store();
        let cases = [
            (
                "generation.create",
                serde_json::json!({"parent": "not-a-generation"}),
                ErrorCode::InvalidArgument,
            ),
            (
                "generation.create",
                serde_json::json!({"parent": GenerationId::new().to_string()}),
                ErrorCode::NotFound,
            ),
            (
                "generation.create",
                serde_json::json!({"reason_goal": GoalId::new().to_string()}),
                ErrorCode::NotFound,
            ),
            (
                "generation.inspect",
                serde_json::json!({}),
                ErrorCode::InvalidArgument,
            ),
            (
                "generation.inspect",
                serde_json::json!({"id": "gen-missing"}),
                ErrorCode::NotFound,
            ),
            (
                "generation.activate",
                serde_json::json!({"id": 42}),
                ErrorCode::InvalidArgument,
            ),
            (
                "generation.promote",
                serde_json::json!({"id": "gen-missing"}),
                ErrorCode::InvalidArgument,
            ),
            (
                "generation.promote",
                serde_json::json!({"id": "gen-missing", "reason": " "}),
                ErrorCode::InvalidArgument,
            ),
            (
                "generation.delete",
                serde_json::json!({}),
                ErrorCode::InvalidArgument,
            ),
        ];
        for (command, args, expected) in cases {
            let error = validate_generation_command(&store, command, &args, TEST_CALLER)
                .unwrap_err()
                .0;
            assert_eq!(error, expected, "{command} {args}");
        }
        store
            .insert_generation(&candidate(None))
            .expect("insert generation");
        assert!(validate_generation_command(
            &store,
            "generation.create",
            &serde_json::json!({}),
            TEST_CALLER
        )
        .is_ok());
    }

    #[test]
    fn direct_generation_runner_preserves_typed_errors() {
        let mut store = migrated_store();
        let missing = GenerationId::new();
        for (name, args, expected) in [
            (
                "generation.create",
                serde_json::json!({"parent": missing}),
                ErrorCode::NotFound,
            ),
            (
                "generation.inspect",
                serde_json::json!({"id": missing}),
                ErrorCode::NotFound,
            ),
            (
                "generation.activate",
                serde_json::json!({"id": missing}),
                ErrorCode::NotFound,
            ),
            (
                "generation.inspect",
                serde_json::json!({}),
                ErrorCode::InvalidArgument,
            ),
            (
                "generation.activate",
                serde_json::json!({"id": 123}),
                ErrorCode::InvalidArgument,
            ),
            (
                "generation.invalid",
                serde_json::json!({}),
                ErrorCode::InvalidArgument,
            ),
        ] {
            let error = run_command(&mut store, name, &args, TEST_CALLER).expect_err(name);
            assert_eq!(error.code, expected, "{name} {args}");
        }
    }

    // --- witness resolution (ADR-0023 G3) ---------------------------------
    // The ledger itself is read and decided in flowerly-core (witness.rs and
    // the lifecycle tests); here only the IPC surface.

    #[test]
    fn resolve_without_a_pending_activation_does_not_read_the_ledger() {
        let mut store = migrated_store();
        let source = LedgerSource {
            path: "/nonexistent/boot-witness.jsonl".into(),
            policy: flowerly_core::witness::LedgerPolicy {
                owner_uid: 0,
                reader_gid: 0,
            },
            boot_id_path: "/nonexistent/boot_id".into(),
        };
        assert_eq!(
            resolve_with(&mut store, &source).unwrap()["outcome"],
            "none"
        );
    }

    #[test]
    fn every_resolution_has_a_distinct_outcome() {
        use flowerly_core::storage::lifecycle::ActivationState;
        let activation = |state| {
            Box::new(Activation {
                id: 7,
                generation: GenerationId::new(),
                previous_generation: None,
                kind: ActivationKind::Activate,
                actor_uid: 0,
                reason: "go".to_owned(),
                operation_id: None,
                request_digest: hash('f'),
                expected_slot: "root-b".to_owned(),
                expected_root_partuuid: "11111111-2222-3333-4444-555555555555".to_owned(),
                ledger_sequence: 2,
                ledger_head: hash('e'),
                irreversible: false,
                backup_sha256: None,
                acknowledges: None,
                state,
                requested_at: Timestamp::now(),
                resolved_at: Some(Timestamp::now()),
                witness_boot_id: None,
                witness_sequence: None,
                detail: None,
            })
        };
        for (resolution, outcome) in [
            (Resolution::NoPending, "none"),
            (
                Resolution::Waiting {
                    activation: 7,
                    reason: "no health".to_owned(),
                },
                "waiting",
            ),
            (
                Resolution::Unresolved {
                    activation: 7,
                    reason: "copied ledger".to_owned(),
                },
                "unresolved",
            ),
            (
                Resolution::Resolved(activation(ActivationState::Confirmed)),
                "confirmed",
            ),
            (
                Resolution::Resolved(activation(ActivationState::FellBack)),
                "fell_back",
            ),
            (
                Resolution::Resolved(activation(ActivationState::Abandoned)),
                "abandoned",
            ),
        ] {
            assert_eq!(resolution_json(resolution)["outcome"], outcome);
        }
    }

    #[test]
    fn resolve_takes_no_arguments_and_needs_a_configured_ledger() {
        let mut store = migrated_store();
        let code = |r: Result<(), (ErrorCode, String)>| r.unwrap_err().0;
        assert_eq!(
            code(validate_generation_command(
                &store,
                "generation.resolve",
                &serde_json::json!([]),
                ADMIN_CALLER
            )),
            ErrorCode::InvalidArgument
        );
        validate_generation_command(
            &store,
            "generation.resolve",
            &serde_json::json!({}),
            ADMIN_CALLER,
        )
        .unwrap();
        let error = run_command(
            &mut store,
            "generation.resolve",
            &serde_json::json!({}),
            ADMIN_CALLER,
        )
        .unwrap_err();
        assert_ne!(error.code, ErrorCode::Internal);
    }

    const ADMIN_CALLER: Caller = Caller::new(4242);
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    fn hash(c: char) -> String {
        c.to_string().repeat(64)
    }

    fn system_candidate(store: &mut Store) -> GenerationId {
        let mut generation = candidate(None);
        generation.owner = Owner::System;
        store.insert_generation(&generation).expect("insert");
        generation.id
    }

    fn identity_args(id: &GenerationId) -> Value {
        serde_json::json!({
            "id": id,
            "source_commit": COMMIT,
            "rootfs_sha256": hash('a'),
            "kernel_sha256": hash('b'),
            "initramfs_sha256": hash('c'),
            "input_manifest_sha256": hash('d'),
            "build_id": "build-1",
            "reason": "built",
        })
    }

    fn evidence_args(id: &GenerationId) -> Value {
        serde_json::json!({
            "id": id, "outcome": "passed", "evidence_sha256": hash('9'), "reason": "kvm run",
        })
    }

    /// Validation and execution together, as the router runs them.
    fn call(store: &mut Store, name: &str, args: &Value) -> Result<Value, (ErrorCode, String)> {
        validate_generation_command(store, name, args, ADMIN_CALLER)?;
        run_command(store, name, args, ADMIN_CALLER).map_err(|e| (e.code, e.message))
    }

    fn make_tested(store: &mut Store, id: &GenerationId) {
        call(store, "generation.identity", &identity_args(id)).expect("identity");
        let evidence = call(store, "generation.evidence", &evidence_args(id)).expect("evidence");
        call(
            store,
            "generation.test",
            &serde_json::json!({"id": id, "experiment": evidence["experiment"], "reason": "ok"}),
        )
        .expect("test");
    }

    fn code_of(result: Result<Value, (ErrorCode, String)>) -> ErrorCode {
        result.expect_err("must be refused").0
    }

    #[test]
    fn identity_evidence_and_test_reach_tested_and_attribute_the_real_caller() {
        let mut store = migrated_store();
        let id = system_candidate(&mut store);
        let mut args = identity_args(&id);
        args["actor_uid"] = serde_json::json!(1);
        call(&mut store, "generation.identity", &args).expect("identity");
        let mut args = evidence_args(&id);
        args["actor_uid"] = serde_json::json!(1);
        let evidence = call(&mut store, "generation.evidence", &args).expect("evidence");
        let done = call(
            &mut store,
            "generation.test",
            &serde_json::json!({"id": id, "experiment": evidence["experiment"], "reason": "ok"}),
        )
        .expect("test");
        assert_eq!(done["status"], "TESTED");
        let audit = store.admin_audit_for_object(&ObjectRef::from(&id)).unwrap();
        assert_eq!(audit.len(), 3);
        assert!(audit.iter().all(|r| r.actor_uid == ADMIN_CALLER.uid()));
    }

    #[test]
    fn tested_is_refused_without_identity_or_passing_evidence() {
        let mut store = migrated_store();
        let id = system_candidate(&mut store);
        let evidence = call(&mut store, "generation.evidence", &evidence_args(&id)).unwrap();
        let args =
            serde_json::json!({"id": id, "experiment": evidence["experiment"], "reason": "x"});
        assert_eq!(
            code_of(call(&mut store, "generation.test", &args)),
            ErrorCode::Conflict
        );

        let other = system_candidate(&mut store);
        call(&mut store, "generation.identity", &identity_args(&other)).unwrap();
        let mut failed = evidence_args(&other);
        failed["outcome"] = serde_json::json!("failed");
        let evidence = call(&mut store, "generation.evidence", &failed).unwrap();
        let args =
            serde_json::json!({"id": other, "experiment": evidence["experiment"], "reason": "x"});
        assert_eq!(
            code_of(call(&mut store, "generation.test", &args)),
            ErrorCode::Conflict
        );
        assert_eq!(
            store.get_generation(&other).unwrap().unwrap().status,
            GenerationStatus::Candidate
        );
    }

    #[test]
    fn lifecycle_commands_reject_malformed_arguments() {
        let mut store = migrated_store();
        let id = system_candidate(&mut store);
        let mut bad_identity = identity_args(&id);
        bad_identity["rootfs_sha256"] = serde_json::json!("xyz");
        let mut no_reason = identity_args(&id);
        no_reason["reason"] = serde_json::json!(" ");
        let mut bad_outcome = evidence_args(&id);
        bad_outcome["outcome"] = serde_json::json!("maybe");
        let mut bad_hash = evidence_args(&id);
        bad_hash["evidence_sha256"] = serde_json::json!("xyz");
        for (name, args) in [
            ("generation.identity", serde_json::json!({})),
            ("generation.identity", bad_identity),
            ("generation.identity", no_reason),
            ("generation.evidence", serde_json::json!({"id": id})),
            ("generation.evidence", bad_outcome),
            ("generation.evidence", bad_hash),
            (
                "generation.test",
                serde_json::json!({"id": id, "reason": "x"}),
            ),
            (
                "generation.test",
                serde_json::json!({"id": id, "experiment": "not-an-id", "reason": "x"}),
            ),
            (
                "generation.test",
                serde_json::json!({"id": id, "experiment": "exp-1", "reason": " "}),
            ),
            ("generation.reject", serde_json::json!({"id": id})),
            (
                "generation.reject",
                serde_json::json!({"id": id, "reason": ""}),
            ),
            (
                "generation.revoke",
                serde_json::json!({"id": "x", "reason": "r"}),
            ),
            ("generation.history", serde_json::json!({"id": "x"})),
            ("generation.history", serde_json::json!({"id": 1})),
            ("generation.rollback", serde_json::json!({})),
            ("generation.rollback", serde_json::json!({"id": "x"})),
            (
                "generation.activate",
                serde_json::json!({"id": id, "reason": " "}),
            ),
            (
                "generation.activate",
                serde_json::json!({"id": id, "operation_id": "a b"}),
            ),
            (
                "generation.rollback",
                serde_json::json!({"id": id, "operation_id": ""}),
            ),
            (
                "generation.activate",
                serde_json::json!({"id": id, "acknowledges": "x"}),
            ),
        ] {
            assert_eq!(
                code_of(call(&mut store, name, &args)),
                ErrorCode::InvalidArgument,
                "{name} {args}"
            );
        }
    }

    #[test]
    fn reject_and_revoke_follow_the_lifecycle() {
        let mut store = migrated_store();
        let rejected = system_candidate(&mut store);
        let args = serde_json::json!({"id": rejected, "reason": "broken"});
        let done = call(&mut store, "generation.reject", &args).expect("reject");
        assert_eq!(done["status"], "REJECTED");
        assert_eq!(
            code_of(call(&mut store, "generation.reject", &args)),
            ErrorCode::Conflict
        );

        let candidate = system_candidate(&mut store);
        let args = serde_json::json!({"id": candidate, "reason": "no"});
        assert_eq!(
            code_of(call(&mut store, "generation.revoke", &args)),
            ErrorCode::Conflict
        );

        let tested = system_candidate(&mut store);
        make_tested(&mut store, &tested);
        let args = serde_json::json!({"id": tested, "reason": "cve"});
        let done = call(&mut store, "generation.revoke", &args).expect("revoke");
        assert_eq!(done["status"], "REVOKED");

        let missing = serde_json::json!({"id": GenerationId::new(), "reason": "x"});
        assert_eq!(
            code_of(call(&mut store, "generation.reject", &missing)),
            ErrorCode::NotFound
        );
    }

    #[test]
    fn activate_and_rollback_refuse_and_write_nothing() {
        let mut store = migrated_store();
        let id = system_candidate(&mut store);
        make_tested(&mut store, &id);
        let args = serde_json::json!({
            "id": id, "reason": "ship", "operation_id": "op-1", "acknowledges": 1,
        });
        let audit_before = store
            .admin_audit_for_object(&ObjectRef::from(&id))
            .unwrap()
            .len();
        for name in ["generation.activate", "generation.rollback"] {
            let (code, message) = call(&mut store, name, &args).expect_err(name);
            assert_eq!(code, ErrorCode::Conflict, "{name}");
            assert!(message.contains("Gate D"), "{message}");
            assert_eq!(
                store.get_generation(&id).unwrap().unwrap().status,
                GenerationStatus::Tested
            );
            assert!(store.activation_history().unwrap().is_empty());
            assert!(store.pending_activation().unwrap().is_none());
        }
        assert_eq!(
            store
                .admin_audit_for_object(&ObjectRef::from(&id))
                .unwrap()
                .len(),
            audit_before
        );
        let missing = serde_json::json!({"id": GenerationId::new()});
        assert_eq!(
            code_of(call(&mut store, "generation.rollback", &missing)),
            ErrorCode::NotFound
        );
    }

    #[test]
    fn history_reports_the_real_actor_and_filters_by_generation() {
        let mut store = migrated_store();
        let empty = call(&mut store, "generation.history", &serde_json::json!({})).unwrap();
        assert_eq!(empty["count"], 0);

        let first = system_candidate(&mut store);
        make_tested(&mut store, &first);
        let request = |id: &GenerationId| ActivationRequest {
            generation: id.clone(),
            kind: ActivationKind::Activate,
            reason: "go".to_owned(),
            operation_id: None,
            expected_slot: "root-b".to_owned(),
            expected_root_partuuid: "11111111-2222-3333-4444-555555555555".to_owned(),
            ledger_sequence: 1,
            ledger_head: hash('e'),
            irreversible: false,
            backup_sha256: None,
            acknowledges: None,
        };
        let row = store
            .request_activation(ADMIN_CALLER, &request(&first))
            .unwrap();
        store.abandon_activation(row.id, "no slot").unwrap();
        let second = system_candidate(&mut store);
        make_tested(&mut store, &second);
        store
            .request_activation(ADMIN_CALLER, &request(&second))
            .unwrap();

        let all = call(&mut store, "generation.history", &serde_json::json!({})).unwrap();
        assert_eq!(all["count"], 2);
        assert_eq!(all["activations"][0]["state"], "ABANDONED");
        assert_eq!(all["activations"][0]["actor_uid"], ADMIN_CALLER.uid());
        assert_eq!(all["activations"][0]["detail"], "no slot");
        assert_eq!(all["activations"][1]["kind"], "ACTIVATE");
        assert_eq!(all["activations"][1]["state"], "PENDING");

        let one = call(
            &mut store,
            "generation.history",
            &serde_json::json!({"id": second}),
        )
        .unwrap();
        assert_eq!(one["count"], 1);
        assert_eq!(one["activations"][0]["generation"], second.as_str());

        let missing = serde_json::json!({"id": GenerationId::new()});
        assert_eq!(
            code_of(call(&mut store, "generation.history", &missing)),
            ErrorCode::NotFound
        );
    }

    #[test]
    fn create_validates_its_optional_text_fields() {
        let mut store = migrated_store();
        for args in [
            serde_json::json!({"source_commit": ""}),
            serde_json::json!({"source_commit": "a b"}),
            serde_json::json!({"source_commit": "a\nb"}),
            serde_json::json!({"source_commit": "a".repeat(MAX_GENERATION_TEXT + 1)}),
            serde_json::json!({"system_manifest": ""}),
            serde_json::json!({"system_manifest": "x\u{7f}"}),
        ] {
            assert_eq!(
                code_of(call(&mut store, "generation.create", &args)),
                ErrorCode::InvalidArgument,
                "{args}"
            );
        }
        assert!(store.list_generations().unwrap().is_empty());
        for args in [
            serde_json::json!({}),
            serde_json::json!({"source_commit": null, "system_manifest": null}),
            serde_json::json!({"source_commit": "abc", "system_manifest": "sha256:deadbeef"}),
            serde_json::json!({"source_commit": COMMIT}),
        ] {
            call(&mut store, "generation.create", &args).expect("legacy fields stay accepted");
        }
    }

    #[test]
    fn activation_is_refused_until_gate_d_is_verified() {
        let mut store = migrated_store();
        let mut first = candidate(None);
        first.owner = Owner::System;
        let second = candidate(Some(first.id.clone()));
        store.insert_generation(&first).expect("insert first");
        store.insert_generation(&second).expect("insert second");

        let missing = run_command(
            &mut store,
            "generation.activate",
            &serde_json::json!({ "id": "gen-missing" }),
            TEST_CALLER,
        )
        .unwrap_err();
        assert_eq!(missing.code, ErrorCode::NotFound);
        for id in [&first.id, &second.id] {
            let refused = run_command(
                &mut store,
                "generation.activate",
                &serde_json::json!({ "id": id }),
                TEST_CALLER,
            )
            .unwrap_err();
            assert_eq!(refused.code, ErrorCode::Conflict);
        }
        let promoted = run_command(
            &mut store,
            "generation.promote",
            &serde_json::json!({ "id": second.id, "reason": "release" }),
            TEST_CALLER,
        )
        .expect("promote second");
        assert_eq!(promoted["owner"], "system");

        // Promotion does not open the path: nothing becomes STABLE.
        let refused = run_command(
            &mut store,
            "generation.activate",
            &serde_json::json!({ "id": second.id }),
            TEST_CALLER,
        )
        .unwrap_err();
        assert_eq!(refused.code, ErrorCode::Conflict);
        for id in [&first.id, &second.id] {
            assert_eq!(
                store.get_generation(id).unwrap().unwrap().status,
                GenerationStatus::Candidate
            );
        }
        assert!(store.activation_history().unwrap().is_empty());
    }
}
