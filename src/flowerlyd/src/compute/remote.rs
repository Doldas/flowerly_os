//! Remote inference behind the gates of ADR-0022 (#263). Disabled by default.
//!
//! A prompt is handed to a remote provider process only when **every**
//! gate holds, all checked under the store lock before anything is sent:
//!
//! 1. The administrator policy file enables remote inference and lists the
//!    registered provider by name (socket, destination, model allowlist).
//! 2. The request says `remote: true`.
//! 3. The workload's resolved profile is `approved-remote-opt-in`.
//! 4. The workload's model is on the provider's allowlist.
//! 5. The caller holds an unrevoked, unexpired consent grant for exactly
//!    this provider and this destination (owner Q3). Changing either the
//!    provider or the configured destination leaves no matching grant.
//! 6. The caller's open attempts at this provider are under the limit.
//!
//! Then the provider process itself must report the same destination over
//! IPC before the prompt is submitted, so a reconfigured provider cannot
//! redirect consented prompts.
//!
//! Every submission first records a durable attempt (`submitting`), so an
//! outcome lost to a crash is reported as `unknown`, never retried. An
//! `operation_id` maps to exactly one attempt: a retry returns that
//! attempt's state and never submits again (unknown outcomes included).
//! Attempts keep metadata only; prompt and response text are never stored
//! (owner Q4). Generated text is relayed from the provider's memory, once
//! by default or within the administrator's short replay window; after
//! that a typed `response_available: false` is returned, never a re-run.
//!
//! The model's text is data and grants nothing.
use std::path::Path;
use std::str::FromStr;
use std::sync::{OnceLock, RwLock};

use flowerly_core::ipc::{Client, ErrorCode, Payload};
use flowerly_core::profile::{FallbackBehavior, Locality};
use flowerly_core::remote_policy::{RemoteGate, RemoteProvider};
use flowerly_core::storage::remote::{AttemptRecord, AttemptState, AttemptUpdate, NewAttempt};
use flowerly_core::storage::{StorageError, Store};
use flowerly_core::types::{Caller, ProviderId, ProviderRecord, Timestamp, WorkloadState};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::inference::{
    load_workload, lock, outage_outcome, valid_request_id, MAX_PROMPT_BYTES, MAX_TOKENS,
};
use super::{profiles, CommandFailure, CommandResult, SharedStore};

pub(crate) const REMOTE_PROVIDER_KIND: &str = "remote-openai-compatible";

fn gate_cell() -> &'static RwLock<RemoteGate> {
    static GATE: OnceLock<RwLock<RemoteGate>> = OnceLock::new();
    GATE.get_or_init(|| RwLock::new(RemoteGate::default()))
}

/// Install the administrator gate loaded at startup.
pub(crate) fn set_gate(gate: RemoteGate) {
    if let Ok(mut current) = gate_cell().write() {
        *current = gate;
    }
}

fn gate() -> RemoteGate {
    gate_cell()
        .read()
        .map(|gate| gate.clone())
        .unwrap_or_else(|_| RemoteGate::Disabled {
            reason: "remote inference gate lock poisoned".to_owned(),
        })
}

/// Milliseconds since the epoch; tests may advance it to cross expiry.
fn now_ms() -> i64 {
    #[cfg(test)]
    {
        Timestamp::now().as_millis() + tests::CLOCK_OFFSET.load(std::sync::atomic::Ordering::SeqCst)
    }
    #[cfg(not(test))]
    {
        Timestamp::now().as_millis()
    }
}

fn denied(message: impl Into<String>) -> CommandFailure {
    CommandFailure {
        code: ErrorCode::PermissionDenied,
        message: message.into(),
    }
}

/// The enabled policy entry for a registered remote provider, plus the
/// replay window, or the reason remote inference is unavailable.
fn entry_for(provider: &ProviderRecord) -> Result<(RemoteProvider, u64), CommandFailure> {
    if provider.kind != REMOTE_PROVIDER_KIND {
        return Err(CommandFailure::invalid(format!(
            "provider {} is not a remote provider",
            provider.id
        )));
    }
    match gate() {
        RemoteGate::Disabled { reason } => Err(denied(reason)),
        RemoteGate::Enabled(policy) => policy
            .provider(&provider.name)
            .cloned()
            .map(|entry| (entry, policy.response_replay_seconds))
            .ok_or_else(|| {
                denied(format!(
                    "provider `{}` is not in the administrator's remote provider policy",
                    provider.name
                ))
            }),
    }
}

/// `provider.register` for the remote kind: the policy must already list
/// the name, and the endpoint comes from the policy, never from arguments.
pub(crate) fn check_registration(name: &str, args: &Value) -> Result<(), CommandFailure> {
    if args
        .get("endpoint")
        .is_some_and(|endpoint| !endpoint.is_null())
    {
        return Err(CommandFailure::invalid(
            "a remote provider's endpoint comes from the administrator policy, not from arguments",
        ));
    }
    match gate() {
        RemoteGate::Disabled { reason } => Err(denied(format!(
            "provider kind `{REMOTE_PROVIDER_KIND}` refused: {reason}"
        ))),
        RemoteGate::Enabled(policy) if policy.provider(name).is_none() => Err(denied(format!(
            "provider kind `{REMOTE_PROVIDER_KIND}` refused: `{name}` is not in the administrator's remote provider policy"
        ))),
        RemoteGate::Enabled(_) => Ok(()),
    }
}

/// The destination shown for a registered provider, if it is remote and
/// currently enabled.
pub(crate) fn destination_of(provider: &ProviderRecord) -> Option<String> {
    entry_for(provider).ok().map(|(entry, _)| entry.destination)
}

pub(crate) fn capabilities() -> Value {
    match gate() {
        RemoteGate::Disabled { reason } => serde_json::json!({"enabled": false, "reason": reason}),
        RemoteGate::Enabled(policy) => serde_json::json!({
            "enabled": true,
            "response_replay_seconds": policy.response_replay_seconds,
            "providers": policy.providers.iter().map(|entry| serde_json::json!({
                "name": entry.name,
                "destination": entry.destination,
                "models": entry.models,
            })).collect::<Vec<_>>(),
        }),
    }
}

fn provider_by_id(store: &Store, raw: Option<&str>) -> Result<ProviderRecord, CommandFailure> {
    let raw = raw.ok_or_else(|| CommandFailure::invalid("missing string arg `provider_id`"))?;
    let id = ProviderId::from_str(raw)
        .map_err(|error| CommandFailure::invalid(format!("invalid provider id: {error}")))?;
    store
        .get_provider(&id)
        .map_err(CommandFailure::storage)?
        .ok_or_else(|| CommandFailure {
            code: ErrorCode::NotFound,
            message: format!("provider not found: {id}"),
        })
}

pub(crate) fn is_remote_request(store: &SharedStore, args: &Value) -> Result<bool, CommandFailure> {
    let guard = lock(store)?;
    let Some(raw) = args.get("provider_id").and_then(Value::as_str) else {
        return Ok(false);
    };
    let Ok(id) = ProviderId::from_str(raw) else {
        return Ok(false);
    };
    Ok(guard
        .get_provider(&id)
        .map_err(CommandFailure::storage)?
        .is_some_and(|provider| provider.kind == REMOTE_PROVIDER_KIND))
}

// --- consent ---------------------------------------------------------------

pub(crate) fn consent(
    store: &SharedStore,
    name: &str,
    args: &Value,
    caller: Caller,
) -> CommandResult {
    let mut store = lock(store)?;
    match name {
        "inference.consent_grant" => {
            let provider = provider_by_id(&store, args.get("provider_id").and_then(Value::as_str))?;
            let (entry, _) = entry_for(&provider)?;
            // Informed consent: the caller must name the destination.
            let destination = args
                .get("destination")
                .and_then(Value::as_str)
                .ok_or_else(|| CommandFailure::invalid("missing string arg `destination`"))?
                .to_ascii_lowercase();
            if destination != entry.destination {
                return Err(CommandFailure::invalid(format!(
                    "`destination` must name the provider's configured destination `{}`",
                    entry.destination
                )));
            }
            let days = match args.get("days") {
                None | Some(Value::Null) => flowerly_core::remote_policy::MAX_CONSENT_DAYS,
                Some(value) => value
                    .as_u64()
                    .ok_or_else(|| CommandFailure::invalid("`days` must be an integer"))?,
            };
            let grant = store
                .grant_remote_consent(caller, provider.id.as_str(), &destination, days)
                .map_err(|error| match error {
                    StorageError::InvalidOperation(message) => CommandFailure::invalid(message),
                    other => CommandFailure::storage(other),
                })?;
            tracing::info!(provider = %provider.id, destination = %destination, consent = %grant.id, "remote inference consent granted");
            Ok(consent_json(&grant, Some(&entry.destination)))
        }
        "inference.consent_revoke" => {
            let consent_id = args.get("consent_id").and_then(Value::as_str);
            let provider_id = args.get("provider_id").and_then(Value::as_str);
            if consent_id.is_none() && provider_id.is_none() {
                return Err(CommandFailure::invalid(
                    "name `consent_id` or `provider_id` to revoke",
                ));
            }
            let revoked = store
                .revoke_remote_consent(caller, consent_id, provider_id)
                .map_err(CommandFailure::storage)?;
            tracing::info!(revoked, "remote inference consent revoked");
            Ok(serde_json::json!({"revoked": revoked}))
        }
        "inference.consent_list" => {
            let grants = store
                .list_remote_consents(caller)
                .map_err(CommandFailure::storage)?;
            let mut items = Vec::with_capacity(grants.len());
            for grant in &grants {
                let current = ProviderId::from_str(&grant.provider_id)
                    .ok()
                    .and_then(|id| store.get_provider(&id).ok().flatten())
                    .and_then(|provider| destination_of(&provider));
                items.push(consent_json(grant, current.as_deref()));
            }
            Ok(serde_json::json!({"consents": items, "count": items.len()}))
        }
        other => Err(CommandFailure::invalid(format!(
            "compute: unknown command `{other}`"
        ))),
    }
}

fn consent_json(
    grant: &flowerly_core::storage::remote::ConsentRecord,
    current_destination: Option<&str>,
) -> Value {
    let active = grant.active_at(now_ms());
    serde_json::json!({
        "id": grant.id,
        "provider_id": grant.provider_id,
        "destination": grant.destination,
        "granted_at": grant.granted_at,
        "expires_at": grant.expires_at,
        "revoked_at": grant.revoked_at,
        "active": active,
        // A grant only covers the destination it names.
        "valid": active && current_destination == Some(grant.destination.as_str()),
    })
}

// --- requests --------------------------------------------------------------

struct Submission {
    attempt_id: String,
    socket: std::path::PathBuf,
    destination: String,
    provider_id: String,
    model: String,
    prompt: String,
    max_tokens: u64,
    replay_seconds: u64,
    fallback: FallbackBehavior,
}

enum Phase {
    Submit(Box<Submission>),
    /// The operation already names an attempt: report it, never resubmit.
    Existing(Box<AttemptRecord>),
}

pub(crate) fn request(store: &SharedStore, args: &Value, caller: Caller) -> CommandResult {
    let phase = {
        let mut guard = lock(store)?;
        prepare(&mut guard, args, caller)?
    };
    let submission = match phase {
        Phase::Existing(attempt) => return status(store, *attempt, caller),
        Phase::Submit(submission) => submission,
    };
    let (state, update, message) = submit(&submission);
    let attempt = {
        let mut guard = lock(store)?;
        guard
            .update_remote_attempt(caller, &submission.attempt_id, state, &update)
            .map_err(CommandFailure::storage)?
    };
    tracing::info!(
        attempt = %attempt.id,
        provider = %submission.provider_id,
        destination = %submission.destination,
        state = attempt.state.as_str(),
        outcome = ?attempt.outcome,
        "remote inference submitted"
    );
    let summary = attempt_json(&attempt);
    match attempt.state {
        // Nothing left the machine: the profile's fallback policy decides.
        AttemptState::Failed if attempt.outcome.as_deref() == Some("not_sent") => {
            let mut outcome = outage_outcome(
                format!(
                    "remote attempt {} not sent: {}",
                    attempt.id,
                    message.unwrap_or_default()
                ),
                submission.fallback,
            )?;
            outcome["attempt"] = summary;
            Ok(outcome)
        }
        _ => Ok(serde_json::json!({
            "executed": false,
            "remote": true,
            "attempt": summary,
            "next": "poll inference.result with the attempt id",
            "error": message,
        })),
    }
}

fn prepare(store: &mut Store, args: &Value, caller: Caller) -> Result<Phase, CommandFailure> {
    let workload = load_workload(store, args, caller)?;
    if workload.state != WorkloadState::Admitted {
        return Err(CommandFailure {
            code: ErrorCode::Conflict,
            message: format!(
                "workload {} is {} (ADMITTED required)",
                workload.id, workload.state
            ),
        });
    }
    let prompt = args
        .get("prompt")
        .and_then(Value::as_str)
        .ok_or_else(|| CommandFailure::invalid("missing string arg `prompt`"))?;
    if prompt.len() > MAX_PROMPT_BYTES {
        return Err(CommandFailure::invalid(format!(
            "`prompt` exceeds {MAX_PROMPT_BYTES} bytes"
        )));
    }
    let max_tokens = match args.get("max_tokens") {
        None | Some(Value::Null) => 128u64,
        Some(value) => value
            .as_u64()
            .filter(|raw| (1..=MAX_TOKENS).contains(raw))
            .ok_or_else(|| {
                CommandFailure::invalid(format!("`max_tokens` must be 1..={MAX_TOKENS}"))
            })?,
    };
    let operation_id = match args.get("operation_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(raw)) if valid_request_id(raw) => Some(raw.as_str()),
        Some(other) => {
            return Err(CommandFailure::invalid(format!(
                "operation_id must follow the idempotency key rules: {other:?}"
            )))
        }
    };
    let provider = provider_by_id(store, args.get("provider_id").and_then(Value::as_str))?;
    // Gate 1: administrator policy.
    let (entry, replay_seconds) = entry_for(&provider)?;
    // Gate 2: the caller asked for remote execution explicitly.
    if args.get("remote") != Some(&Value::Bool(true)) {
        return Err(denied(format!(
            "this prompt would be sent to {}; repeat the request with `remote: true` to confirm",
            entry.destination
        )));
    }
    // Gate 3: the profile allows remote placement.
    let spec = profiles::resolve_effective(store, caller, &workload.profile_id)?.1;
    if spec.locality != Locality::ApprovedRemoteOptIn {
        return Err(denied(
            "the workload's profile is local-only; remote inference needs locality = \"approved-remote-opt-in\"",
        ));
    }
    // Gate 4: the model is on the provider's allowlist.
    let model = store
        .get_model(&workload.model_id)
        .map_err(CommandFailure::storage)?
        .ok_or_else(|| CommandFailure {
            code: ErrorCode::NotFound,
            message: format!("model not found: {}", workload.model_id),
        })?
        .name;
    if !entry.models.contains(&model) {
        return Err(denied(format!(
            "model `{model}` is not on the allowlist of remote provider `{}`",
            entry.name
        )));
    }
    let digest = hex(&Sha256::digest(
        serde_json::to_vec(&serde_json::json!([
            workload.id.as_str(),
            provider.id.as_str(),
            model,
            prompt,
            max_tokens
        ]))
        .map_err(|error| CommandFailure::invalid(error.to_string()))?,
    ));
    // An operation names one attempt forever: report it, never resubmit.
    if let Some(operation_id) = operation_id {
        if let Some(existing) = store
            .remote_attempt_by_operation(caller, operation_id)
            .map_err(CommandFailure::storage)?
        {
            if existing.request_digest != digest {
                return Err(CommandFailure {
                    code: ErrorCode::Conflict,
                    message: "operation_id was already used with different request arguments"
                        .to_owned(),
                });
            }
            return Ok(Phase::Existing(Box::new(existing)));
        }
    }
    // Gate 5: consent for this provider and destination, checked before
    // anything is recorded or sent.
    let consent = store
        .active_remote_consent(caller, provider.id.as_str(), &entry.destination, now_ms())
        .map_err(CommandFailure::storage)?
        .ok_or_else(|| {
            denied(format!(
                "no active consent to send your prompts to {} via provider {}; grant one with inference.consent_grant",
                entry.destination, provider.id
            ))
        })?;
    // Gate 6: per-user bound on open attempts.
    let open = store
        .count_open_remote_attempts(caller, provider.id.as_str())
        .map_err(CommandFailure::storage)?;
    if open >= entry.max_in_flight_per_user {
        return Err(CommandFailure {
            code: ErrorCode::Unavailable,
            message: format!(
                "{open} remote attempts already open at this provider (limit {}); poll them first",
                entry.max_in_flight_per_user
            ),
        });
    }
    let fallback = spec.fallback;
    let attempt_id = format!("rattempt-{}", uuid::Uuid::new_v4().simple());
    store
        .insert_remote_attempt(
            caller,
            &NewAttempt {
                id: &attempt_id,
                workload_id: workload.id.as_str(),
                provider_id: provider.id.as_str(),
                destination: &entry.destination,
                model: &model,
                consent_id: &consent.id,
                operation_id,
                request_id: &attempt_id,
                request_digest: &digest,
            },
        )
        .map_err(CommandFailure::storage)?;
    Ok(Phase::Submit(Box::new(Submission {
        attempt_id,
        socket: entry.socket,
        destination: entry.destination,
        provider_id: provider.id.as_str().to_owned(),
        model,
        prompt: prompt.to_owned(),
        max_tokens,
        replay_seconds,
        fallback,
    })))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The outcome of one IPC exchange with the provider process.
enum Exchange {
    Reply(Payload),
    /// Could not connect: nothing was handed to the provider.
    NotConnected(String),
    /// Connected but the exchange failed: the request may have arrived.
    Lost(String),
}

fn exchange(socket: &Path, payload: Payload) -> Exchange {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return Exchange::NotConnected(error.to_string()),
    };
    runtime.block_on(async {
        let mut client = match Client::connect(socket).await {
            Ok(client) => client,
            Err(error) => return Exchange::NotConnected(error.to_string()),
        };
        match client.request(payload).await {
            Ok(response) => Exchange::Reply(response.payload),
            Err(error) => Exchange::Lost(error.to_string()),
        }
    })
}

fn failed(outcome: &str, class: &str) -> AttemptUpdate {
    AttemptUpdate {
        outcome: Some(outcome.to_owned()),
        error_class: Some(class.to_owned()),
        ..AttemptUpdate::default()
    }
}

/// Parse the `[class; outcome X]` suffix providerd appends to its errors.
fn classify(message: &str) -> (String, String) {
    let tail = message
        .rsplit_once('[')
        .map(|(_, tail)| tail.trim_end_matches(']'));
    if let Some((class, outcome)) = tail.and_then(|tail| tail.split_once("; outcome ")) {
        let outcome = match outcome {
            "not_sent" | "sent" | "unknown" => outcome,
            _ => "unknown",
        };
        return (class.chars().take(64).collect(), outcome.to_owned());
    }
    ("provider_error".to_owned(), "unknown".to_owned())
}

fn state_for(outcome: &str) -> AttemptState {
    if outcome == "unknown" {
        AttemptState::Unknown
    } else {
        AttemptState::Failed
    }
}

/// Confirm the provider's identity, then submit. Runs without the store
/// lock; the attempt is already durable.
fn submit(submission: &Submission) -> (AttemptState, AttemptUpdate, Option<String>) {
    // The provider process must report the destination the caller
    // consented to before it receives the prompt.
    match exchange(&submission.socket, Payload::Status) {
        Exchange::Reply(Payload::Ok { data }) => {
            let details = &data["details"];
            if details["provider"] != "openai-compatible"
                || details["destination"].as_str() != Some(submission.destination.as_str())
            {
                return (
                    AttemptState::Failed,
                    failed("not_sent", "destination_mismatch"),
                    Some(format!(
                        "the provider process does not report destination {}; nothing was sent",
                        submission.destination
                    )),
                );
            }
        }
        Exchange::Reply(_) | Exchange::Lost(_) | Exchange::NotConnected(_) => {
            return (
                AttemptState::Failed,
                failed("not_sent", "provider_unreachable"),
                Some("the remote provider process is unreachable; nothing was sent".to_owned()),
            );
        }
    }
    let payload = Payload::Command {
        name: "provider.submit".to_owned(),
        args: serde_json::json!({
            "request_id": submission.attempt_id,
            "prompt": submission.prompt,
            "max_tokens": submission.max_tokens,
            "model": submission.model,
            "replay_seconds": submission.replay_seconds,
        }),
    };
    match exchange(&submission.socket, payload) {
        Exchange::Reply(Payload::Ok { .. }) => {
            (AttemptState::Pending, AttemptUpdate::default(), None)
        }
        Exchange::Reply(Payload::Err { message, .. }) => {
            let (class, outcome) = classify(&message);
            let state = state_for(&outcome);
            (state, failed(&outcome, &class), Some(message))
        }
        Exchange::Reply(_) => (
            AttemptState::Unknown,
            failed("unknown", "protocol"),
            Some("unexpected provider reply; the request may have been submitted".to_owned()),
        ),
        Exchange::NotConnected(_) => (
            AttemptState::Failed,
            failed("not_sent", "provider_unreachable"),
            Some("the remote provider process is unreachable; nothing was sent".to_owned()),
        ),
        Exchange::Lost(_) => (
            AttemptState::Unknown,
            failed("unknown", "provider_lost"),
            Some(
                "the provider exchange failed after sending; the request may have executed"
                    .to_owned(),
            ),
        ),
    }
}

// --- results ---------------------------------------------------------------

pub(crate) fn result(store: &SharedStore, args: &Value, caller: Caller) -> CommandResult {
    let attempt_id = args
        .get("attempt_id")
        .and_then(Value::as_str)
        .ok_or_else(|| CommandFailure::invalid("missing string arg `attempt_id`"))?;
    let attempt = lock(store)?
        .remote_attempt_for(caller, attempt_id)
        .map_err(CommandFailure::storage)?
        .ok_or_else(|| CommandFailure {
            code: ErrorCode::NotFound,
            message: format!("remote inference attempt not found: {attempt_id}"),
        })?;
    status(store, attempt, caller)
}

/// Report an attempt, polling the provider for news. Never submits.
fn status(store: &SharedStore, attempt: AttemptRecord, caller: Caller) -> CommandResult {
    // A failed or unknown attempt has nothing more to learn.
    if matches!(attempt.state, AttemptState::Failed | AttemptState::Unknown) {
        return Ok(final_json(&attempt, None));
    }
    let entry = {
        let guard = lock(store)?;
        let provider = provider_by_id(&guard, Some(&attempt.provider_id))?;
        entry_for(&provider).ok()
    };
    let Some((entry, _)) = entry else {
        let mut reply = final_json(&attempt, None);
        reply["provider_reachable"] = Value::Bool(false);
        return Ok(reply);
    };
    let poll = Payload::Command {
        name: "provider.poll".to_owned(),
        args: serde_json::json!({"request_id": attempt.request_id}),
    };
    let reply = match exchange(&entry.socket, poll) {
        Exchange::Reply(payload) => payload,
        Exchange::NotConnected(_) | Exchange::Lost(_) => {
            // The provider may come back; the attempt stays as it is.
            let mut reply = final_json(&attempt, None);
            reply["provider_reachable"] = Value::Bool(false);
            return Ok(reply);
        }
    };
    let (next, update, text) = match reply {
        Payload::Ok { data } => match data["state"].as_str() {
            Some("pending") => (AttemptState::Pending, AttemptUpdate::default(), None),
            Some("succeeded") => {
                let result = &data["result"];
                let usage = &result["usage"];
                (
                    AttemptState::Succeeded,
                    AttemptUpdate {
                        outcome: Some("sent".to_owned()),
                        error_class: None,
                        model_used: result["model_used"].as_str().map(str::to_owned),
                        usage_source: usage["source"].as_str().map(str::to_owned),
                        tokens_in: usage["tokens_in"].as_u64(),
                        tokens_out: usage["tokens_out"].as_u64(),
                        latency_ms: result["latency_ms"].as_u64(),
                    },
                    Some(result.clone()),
                )
            }
            Some(state @ ("failed" | "unknown")) => {
                let error = &data["error"];
                let outcome = error["outcome"].as_str().unwrap_or("unknown");
                let class = error["class"].as_str().unwrap_or("provider_error");
                let next = if state == "unknown" {
                    AttemptState::Unknown
                } else {
                    state_for(outcome)
                };
                (next, failed(outcome, class), None)
            }
            _ => (AttemptState::Unknown, failed("unknown", "protocol"), None),
        },
        // The provider has no record (restart or expiry): the outcome of an
        // open attempt is unknown; a finished one keeps its state.
        Payload::Err { .. } => (AttemptState::Unknown, failed("unknown", "lost"), None),
        _ => (AttemptState::Unknown, failed("unknown", "protocol"), None),
    };
    let unchanged = next == AttemptState::Pending && attempt.state == AttemptState::Pending;
    let attempt = if attempt.state.is_final() || unchanged {
        attempt
    } else {
        lock(store)?
            .update_remote_attempt(caller, &attempt.id, next, &update)
            .map_err(CommandFailure::storage)?
    };
    Ok(final_json(&attempt, text.as_ref()))
}

/// The caller-facing view of an attempt. Text is included only when the
/// provider still holds it; otherwise `response_available` says so.
fn final_json(attempt: &AttemptRecord, result: Option<&Value>) -> Value {
    let mut reply = serde_json::json!({
        "executed": attempt.state == AttemptState::Succeeded,
        "remote": true,
        "attempt": attempt_json(attempt),
        "resubmitted": false,
    });
    if attempt.state == AttemptState::Succeeded {
        match result
            .and_then(|result| result.get("text"))
            .and_then(Value::as_str)
        {
            Some(text) => {
                reply["response_available"] = Value::Bool(true);
                reply["text"] = Value::String(text.to_owned());
                reply["truncated"] = result.map_or(Value::Null, |r| r["truncated"].clone());
                reply["finish_reason"] = result.map_or(Value::Null, |r| r["finish_reason"].clone());
            }
            None => {
                reply["response_available"] = Value::Bool(false);
                reply["response_unavailable_reason"] = Value::String(
                    "the generated text was delivered and is not retained; it is not regenerated"
                        .to_owned(),
                );
            }
        }
    }
    if attempt.state == AttemptState::Unknown {
        reply["note"] = Value::String(
            "the request may have executed and been billed; it is never retried automatically. Use a new operation_id to run it again (possible duplicate charge).".to_owned(),
        );
    }
    reply
}

fn attempt_json(attempt: &AttemptRecord) -> Value {
    serde_json::json!({
        "id": attempt.id,
        "workload_id": attempt.workload_id,
        "provider_id": attempt.provider_id,
        "destination": attempt.destination,
        "model": attempt.model,
        "operation_id": attempt.operation_id,
        "state": attempt.state.as_str(),
        "outcome": attempt.outcome,
        "error_class": attempt.error_class,
        "model_used": attempt.model_used,
        "usage": {
            "source": attempt.usage_source,
            "tokens_in": attempt.tokens_in,
            "tokens_out": attempt.tokens_out,
            "cost_micros": Value::Null,
        },
        "latency_ms": attempt.latency_ms,
        "created_at": attempt.created_at,
        "updated_at": attempt.updated_at,
    })
}

#[cfg(test)]
mod tests {
    // Remote inference gates against a fake provider served by the core IPC
    // runner (the reviewed transport). The fake speaks providerd's
    // submit/poll protocol and reports a destination in its Status, so every
    // gate, outcome and retention rule is exercised without any network.
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard};

    use flowerly_core::ipc::{Envelope, ErrorCode, Payload};
    use flowerly_core::profile::BackgroundActivity;
    use flowerly_core::remote_policy::{RemoteGate, RemoteProvider, RemoteProviders};
    use flowerly_core::storage::remote::NewAttempt;
    use flowerly_core::storage::Store;
    use flowerly_core::types::{Caller, Timestamp, WorkloadId, WorkloadRecord, WorkloadState};
    use serde_json::Value;

    use super::super::SharedStore;

    /// Advances `now_ms()` for consent-expiry tests.
    pub(super) static CLOCK_OFFSET: AtomicI64 = AtomicI64::new(0);

    const ALICE: Caller = Caller::new(1001);
    const BOB: Caller = Caller::new(1002);
    const DESTINATION: &str = "openui.example.org";
    const MODEL: &str = "gpt-oss:20b";
    const DAY_MS: i64 = 86_400_000;

    /// The gate and clock are process-wide: remote tests run one at a time.
    fn serial() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        CLOCK_OFFSET.store(0, Ordering::SeqCst);
        guard
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        /// Accept, then succeed; text is delivered once, as providerd does by
        /// default.
        Succeed,
        /// Accept, then report an unknown outcome (for example a timeout).
        Unknown,
        /// Accept, then forget the job (a provider restart).
        Forget,
        /// Accept and stay pending.
        Pending,
        /// Refuse the submit before anything is sent.
        RefuseNotSent,
    }

    struct Fake {
        socket: PathBuf,
        submits: AtomicUsize,
        prompts: Mutex<Vec<String>>,
        delivered: Mutex<Vec<String>>,
        mode: Mutex<Mode>,
    }

    impl Fake {
        fn submits(&self) -> usize {
            self.submits.load(Ordering::SeqCst)
        }

        fn set_mode(&self, mode: Mode) {
            *self.mode.lock().unwrap() = mode;
        }
    }

    const GENERATED: &str = "flowerly-ready";

    fn start_fake(
        dir: &std::path::Path,
        service: &str,
        reported_destination: &str,
        mode: Mode,
    ) -> Arc<Fake> {
        let fake = Arc::new(Fake {
            socket: dir.join(format!("{service}.sock")),
            submits: AtomicUsize::new(0),
            prompts: Mutex::new(Vec::new()),
            delivered: Mutex::new(Vec::new()),
            mode: Mutex::new(mode),
        });
        let state = Arc::clone(&fake);
        let handler = move |request: &Envelope| -> Envelope {
            let Payload::Command { name, args } = &request.payload else {
                return flowerly_core::coded_error_envelope(
                    request.id.clone(),
                    ErrorCode::InvalidArgument,
                    "command",
                );
            };
            let request_id = args["request_id"].as_str().unwrap_or_default().to_owned();
            let mode = *state.mode.lock().unwrap();
            let reply = match name.as_str() {
                "provider.submit" => {
                    if mode == Mode::RefuseNotSent {
                        return flowerly_core::coded_error_envelope(
                            request.id.clone(),
                            ErrorCode::Unavailable,
                            "provider concurrency limit reached [rate_limited; outcome not_sent]",
                        );
                    }
                    state.submits.fetch_add(1, Ordering::SeqCst);
                    state
                        .prompts
                        .lock()
                        .unwrap()
                        .push(args["prompt"].as_str().unwrap_or_default().to_owned());
                    serde_json::json!({"request_id": request_id, "state": "pending", "sent": false})
                }
                "provider.poll" => match mode {
                    Mode::Pending => {
                        serde_json::json!({"request_id": request_id, "state": "pending", "sent": true})
                    }
                    Mode::Unknown => {
                        serde_json::json!({"request_id": request_id, "state": "unknown",
                        "error": {"code": "unavailable", "class": "timeout", "outcome": "unknown"}})
                    }
                    Mode::Forget => {
                        return flowerly_core::coded_error_envelope(
                            request.id.clone(),
                            ErrorCode::NotFound,
                            "no retained job [unknown_request; outcome unknown]",
                        )
                    }
                    Mode::Succeed | Mode::RefuseNotSent => {
                        let mut delivered = state.delivered.lock().unwrap();
                        let first = !delivered.contains(&request_id);
                        delivered.push(request_id.clone());
                        let mut result = serde_json::json!({
                            "model_used": MODEL, "latency_ms": 42, "truncated": false, "finish_reason": "stop",
                            "usage": {"source": "provider_reported", "tokens_in": 12, "tokens_out": 3},
                        });
                        if first {
                            result["text"] = GENERATED.into();
                        } else {
                            result["text_released"] = true.into();
                        }
                        serde_json::json!({"request_id": request_id, "state": "succeeded", "result": result})
                    }
                },
                other => panic!("unexpected provider command {other}"),
            };
            flowerly_core::ok_envelope(request.id.clone(), reply)
        };
        let config = flowerly_core::ServiceConfig {
            name: service.to_owned(),
            runtime_dir: dir.to_path_buf(),
            ..flowerly_core::ServiceConfig::default()
        };
        let status = serde_json::json!({"provider": "openai-compatible", "destination": reported_destination});
        std::thread::spawn(move || {
            tokio::runtime::Runtime::new()
                .expect("runtime")
                .block_on(flowerly_core::run_service(
                    config,
                    status,
                    Arc::new(handler),
                ))
                .expect("fake provider");
        });
        for _ in 0..200 {
            if fake.socket.exists() {
                return fake;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("fake provider did not start");
    }

    fn entry(name: &str, socket: &std::path::Path, destination: &str) -> RemoteProvider {
        RemoteProvider {
            name: name.to_owned(),
            socket: socket.to_path_buf(),
            destination: destination.to_owned(),
            models: vec![MODEL.to_owned()],
            max_in_flight_per_user: 2,
        }
    }

    fn enable(providers: Vec<RemoteProvider>) {
        super::set_gate(RemoteGate::Enabled(Arc::new(RemoteProviders {
            providers,
            response_replay_seconds: 0,
        })));
    }

    fn call(
        store: &SharedStore,
        caller: Caller,
        name: &str,
        args: Value,
    ) -> Result<Value, (ErrorCode, String)> {
        let request = Envelope {
            version: 1,
            id: "test".to_owned(),
            payload: Payload::Command {
                name: name.to_owned(),
                args,
            },
        };
        match super::super::handle_command(store, &request, caller).payload {
            Payload::Ok { data } => Ok(data),
            Payload::Err { code, message } => Err((code.expect("typed"), message)),
            other => panic!("unexpected {other:?}"),
        }
    }

    struct World {
        _dir: tempfile::TempDir,
        dir: PathBuf,
        db: PathBuf,
        store: SharedStore,
        fake: Arc<Fake>,
        provider: String,
        model: String,
        remote_profile: String,
        workload: String,
    }

    impl World {
        /// Remote enabled for provider `gamitron`, Alice holding an admitted
        /// workload on a remote-opt-in profile; no consent yet.
        fn new(mode: Mode) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().to_path_buf();
            let fake = start_fake(&path, "gamitron", DESTINATION, mode);
            enable(vec![entry("gamitron", &fake.socket, DESTINATION)]);
            let db = path.join("state.db");
            let mut store = Store::open(&db).unwrap();
            store.migrate().unwrap();
            let store: SharedStore = Arc::new(std::sync::Mutex::new(store));
            let model = call(
                &store,
                ALICE,
                "model.register",
                serde_json::json!({"name": MODEL, "created_by": "uid:1001"}),
            )
            .unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned();
            let provider = call(
                &store,
                ALICE,
                "provider.register",
                serde_json::json!({"name": "gamitron", "kind": "remote-openai-compatible", "capabilities": ["text"]}),
            )
            .unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned();
            let mut world = Self {
                _dir: dir,
                dir: path,
                db,
                store,
                fake,
                provider,
                model,
                remote_profile: String::new(),
                workload: String::new(),
            };
            world.remote_profile = world.profile(
                ALICE,
                "remote",
                "locality = 'approved-remote-opt-in'\nfallback = 'fail'\n",
            );
            world.workload = world.workload_for(ALICE, &world.remote_profile.clone());
            world
        }

        fn profile(&self, caller: Caller, name: &str, toml: &str) -> String {
            call(
                &self.store,
                caller,
                "profile.create",
                serde_json::json!({"name": name, "toml": toml}),
            )
            .unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned()
        }

        /// Inserted directly: admission measures the host.
        fn workload_for(&self, caller: Caller, profile: &str) -> String {
            let workload = WorkloadRecord {
                id: WorkloadId::new(),
                profile_id: profile.parse().unwrap(),
                profile_name: "p".to_owned(),
                model_id: self.model.parse().unwrap(),
                background: BackgroundActivity::default(),
                priority: 0,
                state: WorkloadState::Admitted,
                reason: "test".to_owned(),
                ceilings: serde_json::json!({}),
                created_by: caller.actor(),
                created_at: Timestamp::now(),
                owner: caller.owner(),
                admitted_at: Some(Timestamp::now()),
                stopped_at: None,
                completed_at: None,
                operation_id: None,
            };
            self.store
                .lock()
                .unwrap()
                .insert_workload(&workload, &[], None)
                .unwrap();
            workload.id.as_str().to_owned()
        }

        fn grant(&self, caller: Caller) -> Result<Value, (ErrorCode, String)> {
            call(
                &self.store,
                caller,
                "inference.consent_grant",
                serde_json::json!({"provider_id": self.provider, "destination": DESTINATION}),
            )
        }

        fn request_as(
            &self,
            caller: Caller,
            workload: &str,
            operation: Option<&str>,
        ) -> Result<Value, (ErrorCode, String)> {
            call(
                &self.store,
                caller,
                "inference.request",
                serde_json::json!({
                    "workload_id": workload, "provider_id": self.provider, "remote": true,
                    "prompt": "Reply with exactly: flowerly-ready (secret-prompt-marker)",
                    "request_id": "client-1", "operation_id": operation, "max_tokens": 32,
                }),
            )
        }

        fn request(&self, operation: Option<&str>) -> Result<Value, (ErrorCode, String)> {
            self.request_as(ALICE, &self.workload, operation)
        }

        fn result(&self, attempt: &str) -> Value {
            call(
                &self.store,
                ALICE,
                "inference.result",
                serde_json::json!({"attempt_id": attempt}),
            )
            .unwrap()
        }
    }

    fn attempt_id(reply: &Value) -> String {
        reply["attempt"]["id"]
            .as_str()
            .expect("attempt id")
            .to_owned()
    }

    fn assert_denied(reply: Result<Value, (ErrorCode, String)>, needle: &str) {
        let (code, message) = reply.expect_err("refused");
        assert_eq!(code, ErrorCode::PermissionDenied, "{message}");
        assert!(
            message.contains(needle),
            "expected `{needle}` in: {message}"
        );
    }

    #[test]
    fn remote_inference_is_disabled_by_default() {
        let _serial = serial();
        super::set_gate(RemoteGate::default());
        let mut db = Store::open_in_memory().unwrap();
        db.migrate().unwrap();
        let store: SharedStore = Arc::new(std::sync::Mutex::new(db));
        let register = |args: Value| call(&store, ALICE, "provider.register", args);
        assert_denied(
            register(
                serde_json::json!({"name": "gamitron", "kind": "remote-openai-compatible", "capabilities": ["text"]}),
            ),
            "no administrator policy",
        );
        let capabilities = super::capabilities();
        assert_eq!(capabilities["enabled"], false);

        let dir = tempfile::tempdir().unwrap();
        enable(vec![entry(
            "gamitron",
            &dir.path().join("g.sock"),
            DESTINATION,
        )]);
        assert_denied(
            register(
                serde_json::json!({"name": "unlisted", "kind": "remote-openai-compatible", "capabilities": ["text"]}),
            ),
            "not in the administrator's remote provider policy",
        );
        let endpoint = register(serde_json::json!({
            "name": "gamitron", "kind": "remote-openai-compatible", "capabilities": ["text"],
            "endpoint": "/tmp/evil.sock",
        }));
        assert_eq!(endpoint.unwrap_err().0, ErrorCode::InvalidArgument);
        // Other unknown kinds keep the original refusal.
        assert_denied(
            register(
                serde_json::json!({"name": "x", "kind": "openai-api", "capabilities": ["text"]}),
            ),
            "no egress policy authorization",
        );
        super::set_gate(RemoteGate::default());
    }

    #[test]
    fn every_gate_is_checked_before_anything_is_sent() {
        let _serial = serial();
        let world = World::new(Mode::Succeed);
        let shown = call(
            &world.store,
            ALICE,
            "provider.show",
            serde_json::json!({"id": world.provider}),
        )
        .unwrap();
        assert_eq!(shown["destination"], DESTINATION);

        // Gate 5: no consent yet.
        assert_denied(world.request(None), "no active consent");
        world.grant(ALICE).unwrap();
        // Gate 2: an explicit `remote: true`.
        let implicit = call(
            &world.store,
            ALICE,
            "inference.request",
            serde_json::json!({"workload_id": world.workload, "provider_id": world.provider, "prompt": "hi", "request_id": "r"}),
        );
        assert_denied(implicit, "remote: true");
        // Gate 3: a local-only profile.
        let local = world.profile(ALICE, "local", "fallback = 'fail'\n");
        let local_workload = world.workload_for(ALICE, &local);
        assert_denied(world.request_as(ALICE, &local_workload, None), "local-only");
        // Gate 4: a model outside the allowlist.
        enable(vec![RemoteProvider {
            models: vec!["other:7b".to_owned()],
            ..entry("gamitron", &world.fake.socket, DESTINATION)
        }]);
        assert_denied(world.request(None), "not on the allowlist");
        // Gate 1: the administrator turns remote inference off.
        super::set_gate(RemoteGate::default());
        assert_denied(world.request(None), "remote inference is disabled");
        assert_eq!(
            world.fake.submits(),
            0,
            "a refused request reached the provider"
        );

        enable(vec![entry("gamitron", &world.fake.socket, DESTINATION)]);
        let submitted = world.request(Some("op-ok")).unwrap();
        assert_eq!(submitted["attempt"]["state"], "pending");
        assert_eq!(submitted["executed"], false);
        let result = world.result(&attempt_id(&submitted));
        assert_eq!(result["executed"], true);
        assert_eq!(result["text"], GENERATED);
        assert_eq!(result["attempt"]["usage"]["tokens_in"], 12);
        assert_eq!(result["attempt"]["usage"]["cost_micros"], Value::Null);
        assert_eq!(world.fake.submits(), 1);
    }

    #[test]
    fn consent_is_per_user_provider_and_destination_and_revocation_is_immediate() {
        let _serial = serial();
        let world = World::new(Mode::Pending);
        // Informed consent: the caller must name the configured destination.
        let wrong = call(
            &world.store,
            ALICE,
            "inference.consent_grant",
            serde_json::json!({"provider_id": world.provider, "destination": "evil.example"}),
        );
        assert_eq!(wrong.unwrap_err().0, ErrorCode::InvalidArgument);
        let too_long = call(
            &world.store,
            ALICE,
            "inference.consent_grant",
            serde_json::json!({"provider_id": world.provider, "destination": DESTINATION, "days": 31}),
        );
        assert_eq!(too_long.unwrap_err().0, ErrorCode::InvalidArgument);

        let grant = world.grant(ALICE).unwrap();
        assert_eq!(grant["valid"], true);
        world.request(None).unwrap();
        assert_eq!(world.fake.submits(), 1);

        // Another user is not covered by Alice's grant.
        let bob_profile = world.profile(BOB, "remote", "locality = 'approved-remote-opt-in'\n");
        let bob_workload = world.workload_for(BOB, &bob_profile);
        assert_denied(
            world.request_as(BOB, &bob_workload, None),
            "no active consent",
        );
        // Nor can Bob revoke it.
        let bob_revoke = call(
            &world.store,
            BOB,
            "inference.consent_revoke",
            serde_json::json!({"consent_id": grant["id"]}),
        );
        assert_eq!(bob_revoke.unwrap()["revoked"], 0);

        // Revocation takes effect for the very next request.
        let revoked = call(
            &world.store,
            ALICE,
            "inference.consent_revoke",
            serde_json::json!({"provider_id": world.provider}),
        );
        assert_eq!(revoked.unwrap()["revoked"], 1);
        assert_denied(world.request(None), "no active consent");
        assert_eq!(world.fake.submits(), 1, "a prompt left after revocation");

        // Expiry: a one-day grant stops covering requests after a day.
        call(
            &world.store,
            ALICE,
            "inference.consent_grant",
            serde_json::json!({"provider_id": world.provider, "destination": DESTINATION, "days": 1}),
        )
        .unwrap();
        world.request(None).unwrap();
        assert_eq!(world.fake.submits(), 2);
        CLOCK_OFFSET.store(DAY_MS + 1, Ordering::SeqCst);
        assert_denied(world.request(None), "no active consent");
        CLOCK_OFFSET.store(0, Ordering::SeqCst);

        // A changed destination invalidates every grant for the old one.
        world.grant(ALICE).unwrap();
        enable(vec![entry(
            "gamitron",
            &world.fake.socket,
            "moved.example.org",
        )]);
        assert_denied(world.request(None), "no active consent");
        let listed = call(
            &world.store,
            ALICE,
            "inference.consent_list",
            serde_json::json!({}),
        )
        .unwrap();
        assert!(
            listed["consents"]
                .as_array()
                .unwrap()
                .iter()
                .all(|grant| grant["valid"] == false),
            "{listed}"
        );

        // A different provider at the same destination needs its own grant.
        enable(vec![
            entry("gamitron", &world.fake.socket, DESTINATION),
            entry("gamitron2", &world.fake.socket, DESTINATION),
        ]);
        let second = call(
            &world.store,
            ALICE,
            "provider.register",
            serde_json::json!({"name": "gamitron2", "kind": "remote-openai-compatible", "capabilities": ["text"]}),
        )
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let other_provider = call(
            &world.store,
            ALICE,
            "inference.request",
            serde_json::json!({"workload_id": world.workload, "provider_id": second, "remote": true,
                "prompt": "hi", "request_id": "r"}),
        );
        assert_denied(other_provider, "no active consent");
        assert_eq!(world.fake.submits(), 2);
    }

    #[test]
    fn a_provider_reporting_another_destination_never_receives_the_prompt() {
        let _serial = serial();
        let world = World::new(Mode::Succeed);
        world.grant(ALICE).unwrap();
        // The process at the socket claims a different destination than the
        // one the policy and the consent name.
        let rogue = start_fake(&world.dir, "rogue", "evil.example", Mode::Succeed);
        enable(vec![entry("gamitron", &rogue.socket, DESTINATION)]);
        let (code, message) = world.request(Some("op-rogue")).expect_err("refused");
        assert_eq!(code, ErrorCode::Unavailable, "{message}");
        assert!(message.contains("not sent"), "{message}");
        assert_eq!(rogue.submits(), 0);
        let attempt = world
            .store
            .lock()
            .unwrap()
            .remote_attempt_by_operation(ALICE, "op-rogue")
            .unwrap()
            .unwrap();
        assert_eq!(attempt.error_class.as_deref(), Some("destination_mismatch"));
        assert_eq!(attempt.outcome.as_deref(), Some("not_sent"));
    }

    #[test]
    fn unknown_outcomes_are_final_and_never_resubmitted() {
        let _serial = serial();
        let world = World::new(Mode::Unknown);
        world.grant(ALICE).unwrap();
        let first = world.request(Some("op-unknown")).unwrap();
        let id = attempt_id(&first);
        let result = world.result(&id);
        assert_eq!(result["attempt"]["state"], "unknown");
        assert_eq!(result["attempt"]["outcome"], "unknown");
        assert!(result["note"].as_str().unwrap().contains("never retried"));
        // A retry of the same operation reports the same attempt and sends nothing.
        world.fake.set_mode(Mode::Succeed);
        let retry = world.request(Some("op-unknown")).unwrap();
        assert_eq!(attempt_id(&retry), id);
        assert_eq!(retry["attempt"]["state"], "unknown");
        assert_eq!(retry["resubmitted"], false);
        assert_eq!(
            world.fake.submits(),
            1,
            "an unknown outcome was resubmitted"
        );
        // Reusing the operation with other arguments is a conflict.
        let changed = call(
            &world.store,
            ALICE,
            "inference.request",
            serde_json::json!({"workload_id": world.workload, "provider_id": world.provider, "remote": true,
                "prompt": "different", "request_id": "r", "operation_id": "op-unknown"}),
        );
        assert_eq!(changed.unwrap_err().0, ErrorCode::Conflict);

        // A provider restart loses the job: unknown, not resubmitted.
        world.fake.set_mode(Mode::Forget);
        let lost = world.request(Some("op-lost")).unwrap();
        let lost = world.result(&attempt_id(&lost));
        assert_eq!(lost["attempt"]["state"], "unknown");
        assert_eq!(lost["attempt"]["error_class"], "lost");

        // A crash after recording the attempt but before submitting: the
        // provider never heard of it, so its outcome is unknown and it is
        // never submitted later.
        let crashed = {
            let mut store = world.store.lock().unwrap();
            let consent = store
                .active_remote_consent(
                    ALICE,
                    &world.provider,
                    DESTINATION,
                    Timestamp::now().as_millis(),
                )
                .unwrap()
                .unwrap();
            store
                .insert_remote_attempt(
                    ALICE,
                    &NewAttempt {
                        id: "rattempt-crashed",
                        workload_id: &world.workload,
                        provider_id: &world.provider,
                        destination: DESTINATION,
                        model: MODEL,
                        consent_id: &consent.id,
                        operation_id: Some("op-crashed"),
                        request_id: "rattempt-crashed",
                        request_digest: &"0".repeat(64),
                    },
                )
                .unwrap();
            "rattempt-crashed"
        };
        let submits = world.fake.submits();
        let recovered = world.result(crashed);
        assert_eq!(recovered["attempt"]["state"], "unknown");
        assert_eq!(world.fake.submits(), submits);
    }

    #[test]
    fn delivered_text_is_not_retained_and_is_never_regenerated() {
        let _serial = serial();
        let world = World::new(Mode::Succeed);
        world.grant(ALICE).unwrap();
        let submitted = world.request(Some("op-text")).unwrap();
        let id = attempt_id(&submitted);
        let first = world.result(&id);
        assert_eq!(first["response_available"], true);
        assert_eq!(first["text"], GENERATED);
        // After delivery (and after any replay window) the answer is an honest
        // typed result with the accounting metadata, never a new execution.
        for again in [world.result(&id), world.request(Some("op-text")).unwrap()] {
            assert_eq!(again["attempt"]["state"], "succeeded");
            assert_eq!(again["response_available"], false, "{again}");
            assert!(again.get("text").is_none());
            assert_eq!(again["resubmitted"], false);
            assert_eq!(again["attempt"]["usage"]["tokens_out"], 3);
        }
        assert_eq!(world.fake.submits(), 1);

        // Owner Q4: neither the prompt nor the generated text is in the
        // database (including its WAL), only metadata.
        drop(world.store.lock().unwrap());
        let mut bytes = std::fs::read(&world.db).unwrap();
        if let Ok(wal) = std::fs::read(world.db.with_extension("db-wal")) {
            bytes.extend(wal);
        }
        let haystack = String::from_utf8_lossy(&bytes);
        assert!(
            !haystack.contains("secret-prompt-marker"),
            "the prompt was stored"
        );
        assert!(
            !haystack.contains(GENERATED),
            "the generated text was stored"
        );
        assert!(haystack.contains(&id), "the attempt metadata is missing");
        assert_eq!(world.fake.prompts.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_refusal_before_sending_applies_the_profile_fallback() {
        let _serial = serial();
        let world = World::new(Mode::RefuseNotSent);
        world.grant(ALICE).unwrap();
        let ask = world.profile(
            ALICE,
            "ask",
            "locality = 'approved-remote-opt-in'\nfallback = 'ask'\n",
        );
        let ask_workload = world.workload_for(ALICE, &ask);
        let reply = world.request_as(ALICE, &ask_workload, None).unwrap();
        assert_eq!(reply["requires_consent"], true);
        assert_eq!(reply["attempt"]["state"], "failed");
        assert_eq!(reply["attempt"]["outcome"], "not_sent");
        assert_eq!(reply["attempt"]["error_class"], "rate_limited");
        let (code, _) = world.request(None).expect_err("fallback = fail");
        assert_eq!(code, ErrorCode::Unavailable);
    }

    #[test]
    fn open_attempts_per_user_are_bounded() {
        let _serial = serial();
        let world = World::new(Mode::Pending);
        world.grant(ALICE).unwrap();
        world.request(None).unwrap();
        world.request(None).unwrap();
        let (code, message) = world.request(None).expect_err("third open attempt");
        assert_eq!(code, ErrorCode::Unavailable, "{message}");
        assert_eq!(world.fake.submits(), 2);
    }

    #[test]
    fn attempts_are_private_to_their_owner() {
        let _serial = serial();
        let world = World::new(Mode::Succeed);
        world.grant(ALICE).unwrap();
        let id = attempt_id(&world.request(None).unwrap());
        let foreign = call(
            &world.store,
            BOB,
            "inference.result",
            serde_json::json!({"attempt_id": id}),
        );
        assert_eq!(foreign.unwrap_err().0, ErrorCode::NotFound);
        let bob = call(
            &world.store,
            BOB,
            "inference.consent_list",
            serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(bob["count"], 0);
    }
}
