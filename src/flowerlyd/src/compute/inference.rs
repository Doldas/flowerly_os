//! Inference execution contract (RFC 0004 tranche 4): flowerlyd fronts the
//! fault-isolated reference `providerd` process over the local authenticated
//! IPC, honors the profile fallback semantics on provider outage, and records
//! the usage ledger with unknown-outcome replay.
//!
//! Contract invariants:
//! - Only **ADMITTED** workloads can execute; queued/refused paths are typed,
//!   never silent. The provider **cannot grant the workload any OS
//!   authority**: the response is data and the only durable write is a
//!   bounded ledger row.
//! - Outage is honest: `fallback = ask` returns `requires_consent` data (no
//!   hidden retry), `queue` explains and stays admitted-neutral, `fail`
//!   produces a typed Unavailable. Flow/Session state is never touched.
//! - Bounded: prompt and max tokens are capped here AND at the provider;
//!   ledger writes carry the client `operation_id` and retries replay the
//!   identical response (#63 semantics).
//! - Provider endpoints must stay inside the daemon runtime directory
//!   (ADR-0010 local boundary); the conventional `providerd.sock` is the
//!   default. No network transport exists by contract.
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::OnceLock;

use flowerly_core::ipc::Payload;
use flowerly_core::profile::FallbackBehavior;
use flowerly_core::storage::{MutationOperation, Store};
use flowerly_core::types::{
    Caller, ObjectRef, ProvenanceEdge, ProvenanceRelation, ProviderId, Timestamp, UsageId,
    WorkloadId, WorkloadRecord, WorkloadState,
};
use serde_json::Value;

use super::{profiles, CommandFailure, CommandResult};

/// Bounded prompt passthrough; providerd enforces the same bound and
/// refuses duplicates, but the runtime keeps requests small by contract.
const MAX_PROMPT_BYTES: usize = 16 * 1024;
const MAX_TOKENS: u64 = 4096;
const DEFAULT_PROVIDER_SOCKET: &str = "providerd.sock";

/// The supervisor's runtime directory, initialized at startup so the
/// bounded worker handlers can address the local provider socket.
static RUNTIME_DIR: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn init_runtime_dir(runtime_dir: PathBuf) {
    let _ = RUNTIME_DIR.set(runtime_dir);
}

fn runtime_dir_snapshot() -> Result<&'static PathBuf, CommandFailure> {
    RUNTIME_DIR.get().ok_or_else(|| CommandFailure {
        code: flowerly_core::ipc::ErrorCode::Unavailable,
        message: "runtime directory not initialized; providers unreachable".to_owned(),
    })
}

pub(crate) fn run_inference(
    store: &mut Store,
    name: &str,
    args: &Value,
    caller: Caller,
) -> CommandResult {
    match name {
        "inference.request" => request(store, args, caller),
        "inference.capabilities" => capabilities(store, args),
        other => Err(CommandFailure::invalid(format!(
            "compute: unknown command `{other}`"
        ))),
    }
}

fn valid_request_id(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 128
        && raw
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
}

fn validate_operation(args: &Value) -> Result<(), CommandFailure> {
    match args.get("operation_id") {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(raw)) if valid_request_id(raw) => Ok(()),
        Some(other) => Err(CommandFailure::invalid(format!(
            "operation_id must follow the idempotency key rules: {other:?}"
        ))),
    }
}

/// One of the caller's own workloads; another uid's is `not_found`.
fn load_workload(
    store: &Store,
    args: &Value,
    caller: Caller,
) -> Result<WorkloadRecord, CommandFailure> {
    let raw = args
        .get("workload_id")
        .and_then(Value::as_str)
        .ok_or_else(|| CommandFailure::invalid("missing string arg `workload_id`"))?;
    let workload_id = WorkloadId::from_str(raw)
        .map_err(|error| CommandFailure::invalid(format!("invalid workload id: {error}")))?;
    store
        .workload_for(caller, &workload_id)
        .map_err(CommandFailure::storage)?
        .ok_or_else(|| CommandFailure {
            code: flowerly_core::ipc::ErrorCode::NotFound,
            message: format!("workload not found: {workload_id}"),
        })
}

fn capabilities(store: &mut Store, args: &Value) -> CommandResult {
    let runtime_dir = runtime_dir_snapshot()?;
    let socket = runtime_dir.join(DEFAULT_PROVIDER_SOCKET);
    let actor = actor_from(args);
    let response = flowerly_core::ipc::call_blocking(
        &socket,
        Payload::Command {
            name: "provider.discover".to_owned(),
            args: serde_json::json!({ "created_by": actor }),
        },
    );
    let discovered = match response {
        Ok(response) => match response.payload {
            Payload::Ok { data } => data,
            Payload::Err { message, .. } => {
                return Err(CommandFailure {
                    code: flowerly_core::ipc::ErrorCode::Unavailable,
                    message: format!("provider discovery refused: {message}"),
                });
            }
            other => {
                return Err(CommandFailure {
                    code: flowerly_core::ipc::ErrorCode::Internal,
                    message: format!("unexpected provider discovery response: {other:?}"),
                });
            }
        },
        Err(error) => {
            return Err(CommandFailure {
                code: flowerly_core::ipc::ErrorCode::Unavailable,
                message: format!("provider discovery unavailable: {error}"),
            });
        }
    };
    let providers = store.list_providers().map_err(CommandFailure::storage)?;
    Ok(serde_json::json!({
        "live_provider": discovered,
        "registered": providers
            .iter()
            .map(|provider| {
                serde_json::json!({
                    "id": provider.id.as_str(),
                    "name": provider.name,
                    "kind": provider.kind,
                })
            })
            .collect::<Vec<_>>(),
        "promise": "reference-provider-only",
    }))
}

fn actor_from(args: &Value) -> String {
    args.get("created_by")
        .and_then(Value::as_str)
        .unwrap_or("user")
        .to_owned()
}

/// Build the honest outage response for the profile's explicit fallback
/// policy. Nothing is retried silently and no state changes.
fn outage_outcome(message: String, fallback: FallbackBehavior) -> CommandResult {
    match fallback {
        FallbackBehavior::Ask => Ok(serde_json::json!({
            "executed": false,
            "requires_consent": true,
            "reason": message,
            "policy": "ask",
        })),
        FallbackBehavior::Queue => Ok(serde_json::json!({
            "executed": false,
            "requires_consent": true,
            "reason": message,
            "policy": "queue",
        })),
        FallbackBehavior::Fail => Err(CommandFailure {
            code: flowerly_core::ipc::ErrorCode::Unavailable,
            message,
        }),
        // Downgrade is refused outright in this tranche: silently replacing
        // the requested contract is exactly what the RFC forbids.
        FallbackBehavior::Downgrade => Err(CommandFailure {
            code: flowerly_core::ipc::ErrorCode::PermissionDenied,
            message: format!("provider outage with `downgrade` fallback is not honored: {message}"),
        }),
    }
}

fn request(store: &mut Store, args: &Value, caller: Caller) -> CommandResult {
    let workload = load_workload(store, args, caller)?;
    if workload.state != WorkloadState::Admitted {
        return Err(CommandFailure {
            code: flowerly_core::ipc::ErrorCode::Conflict,
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
    let request_id = args
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|raw| valid_request_id(raw))
        .ok_or_else(|| {
            CommandFailure::invalid("missing or malformed `request_id` (bounded idempotency rules)")
        })?
        .to_owned();
    validate_operation(args)?;
    let provider_id = args
        .get("provider_id")
        .and_then(Value::as_str)
        .ok_or_else(|| CommandFailure::invalid("missing string arg `provider_id`"))?;
    let provider_id = ProviderId::from_str(provider_id)
        .map_err(|error| CommandFailure::invalid(format!("invalid provider id: {error}")))?;
    let provider = store
        .get_provider(&provider_id)
        .map_err(CommandFailure::storage)?
        .ok_or_else(|| CommandFailure {
            code: flowerly_core::ipc::ErrorCode::NotFound,
            message: format!("provider not found: {provider_id}"),
        })?;

    // An unresolvable profile is an error, never a silently assumed policy.
    let fallback = profiles::resolve_effective(store, caller, &workload.profile_id)?
        .1
        .fallback;

    let runtime_dir = runtime_dir_snapshot()?;
    let socket = runtime_dir.join(
        provider
            .endpoint
            .as_deref()
            .unwrap_or(DEFAULT_PROVIDER_SOCKET),
    );
    let actor = workload.created_by.clone();
    let response = flowerly_core::ipc::call_blocking(
        &socket,
        Payload::Command {
            name: "provider.generate".to_owned(),
            args: serde_json::json!({
                "request_id": request_id,
                "prompt": prompt,
                "max_tokens": max_tokens,
                "created_by": actor,
            }),
        },
    );
    let generated = match response {
        Ok(response) => match response.payload {
            Payload::Ok { data } => data,
            Payload::Err { message, .. } => {
                return outage_outcome(format!("provider refused: {message}"), fallback);
            }
            other => {
                return Err(CommandFailure {
                    code: flowerly_core::ipc::ErrorCode::Internal,
                    message: format!("unexpected provider response type: {other:?}"),
                });
            }
        },
        Err(error) => {
            return outage_outcome(format!("provider unreachable: {error}"), fallback);
        }
    };

    // Ledger the bounded result with unknown-outcome replay. The provider
    // response grants nothing: the only durable artifact is this row.
    let usage_id = UsageId::new();
    let usage = flowerly_core::types::UsageRecord {
        id: usage_id.clone(),
        workload_id: workload.id.clone(),
        model_id: workload.model_id.clone(),
        tokens_in: generated
            .get("tokens_in")
            .and_then(Value::as_u64)
            .ok_or_else(|| CommandFailure {
                code: flowerly_core::ipc::ErrorCode::Internal,
                message: "provider result missing bounded token counts".to_owned(),
            })?,
        tokens_out: generated
            .get("tokens_out")
            .and_then(Value::as_u64)
            .ok_or_else(|| CommandFailure {
                code: flowerly_core::ipc::ErrorCode::Internal,
                message: "provider result missing bounded token counts".to_owned(),
            })?,
        // Unknown latency is never recorded as a measured zero.
        latency_ms: generated
            .get("latency_ms")
            .and_then(Value::as_u64)
            .ok_or_else(|| CommandFailure {
                code: flowerly_core::ipc::ErrorCode::Internal,
                message: "provider result missing measured latency".to_owned(),
            })?,
        cost_micros: 0, // reference provider is cost-free; accounting covers real providers later
        created_by: actor.clone(),
        created_at: Timestamp::now(),
        // Usage belongs to the owner of the workload it accounts for.
        owner: workload.owner,
    };
    let intended = serde_json::json!({
        "usage_id": usage_id.as_str(),
        "workload_id": workload.id.as_str(),
        "request_id": request_id,
        "provider": provider.id.as_str(),
        "executed": true,
        "result": generated,
    });
    let response_value =
        if let Some(operation_id) = args.get("operation_id").and_then(Value::as_str) {
            let request_json = serde_json::to_string(args)
                .map_err(|error| CommandFailure::invalid(error.to_string()))?;
            let response_json = serde_json::to_string(&intended)
                .map_err(|error| CommandFailure::invalid(error.to_string()))?;
            let operation = MutationOperation {
                actor: &actor,
                command: "inference.request",
                operation_id,
                request_payload: &request_json,
                response_payload: &response_json,
            };
            let stored = store
                .insert_usage_once(&usage, operation)
                .map_err(CommandFailure::storage)?;
            if stored.is_empty() {
                intended
            } else {
                serde_json::from_str(&stored)
                    .map_err(|error| CommandFailure::invalid(error.to_string()))?
            }
        } else {
            store
                .insert_usage(&usage)
                .map_err(CommandFailure::storage)?;
            store
                .insert_provenance(&ProvenanceEdge {
                    from: ObjectRef::from(&provider.id),
                    relation: ProvenanceRelation::Produced,
                    to: ObjectRef::from(&workload.id),
                    created_at: Timestamp::now(),
                    note: Some("reference provider usage ledger".to_owned()),
                })
                .map_err(CommandFailure::storage)?;
            intended
        };
    tracing::info!(
        workload = %workload.id,
        usage = %usage.id,
        "inference request recorded via reference provider"
    );
    Ok(response_value)
}
