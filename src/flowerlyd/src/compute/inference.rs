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
//!   identical response (#63 semantics) — checked before the provider is
//!   called, so a retry never executes twice.
//! - The shared store lock is never held across a provider call.
//! - Provider endpoints must stay inside the daemon runtime directory
//!   (ADR-0010 local boundary); the conventional `providerd.sock` is the
//!   default. No network transport exists by contract.
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{MutexGuard, OnceLock};

use flowerly_core::ipc::Payload;
use flowerly_core::profile::FallbackBehavior;
use flowerly_core::storage::{MutationOperation, Store};
use flowerly_core::types::{
    Caller, ObjectRef, ProvenanceEdge, ProvenanceRelation, ProviderId, Timestamp, UsageId,
    WorkloadId, WorkloadRecord, WorkloadState,
};
use serde_json::Value;

use super::{profiles, CommandFailure, CommandResult, SharedStore};

/// Bounded prompt passthrough; providerd enforces the same bound and
/// refuses duplicates, but the runtime keeps requests small by contract.
pub(super) const MAX_PROMPT_BYTES: usize = 16 * 1024;
pub(super) const MAX_TOKENS: u64 = 4096;
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

/// Inference commands take the shared store, not a held guard: a provider
/// call must never run under the store lock, or one slow provider would
/// stall every other module (Flows, Generations, provenance).
pub(crate) fn run_inference(
    store: &SharedStore,
    name: &str,
    args: &Value,
    caller: Caller,
) -> CommandResult {
    match name {
        "inference.request" if super::remote::is_remote_request(store, args)? => {
            super::remote::request(store, args, caller)
        }
        "inference.request" => request(store, args, caller),
        "inference.result" => super::remote::result(store, args, caller),
        "inference.consent_grant" | "inference.consent_revoke" | "inference.consent_list" => {
            super::remote::consent(store, name, args, caller)
        }
        "inference.capabilities" => capabilities(store, args),
        other => Err(CommandFailure::invalid(format!(
            "compute: unknown command `{other}`"
        ))),
    }
}

pub(super) fn lock(store: &SharedStore) -> Result<MutexGuard<'_, Store>, CommandFailure> {
    store.lock().map_err(|_| CommandFailure {
        code: flowerly_core::ipc::ErrorCode::Internal,
        message: "storage lock poisoned".to_owned(),
    })
}

pub(super) fn valid_request_id(raw: &str) -> bool {
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
pub(super) fn load_workload(
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

fn capabilities(store: &SharedStore, args: &Value) -> CommandResult {
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
    let providers = lock(store)?
        .list_providers()
        .map_err(CommandFailure::storage)?;
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
        "remote": super::remote::capabilities(),
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
pub(super) fn outage_outcome(message: String, fallback: FallbackBehavior) -> CommandResult {
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

/// Everything `request` resolves under the store lock before the provider
/// call, so the call itself runs unlocked.
struct Prepared {
    workload: WorkloadRecord,
    provider_id: ProviderId,
    fallback: FallbackBehavior,
    socket: PathBuf,
    request_id: String,
    prompt: String,
    max_tokens: u64,
    /// `(operation_id, serialized request)` when the caller asked for replay.
    operation: Option<(String, String)>,
}

enum Phase {
    Execute(Box<Prepared>),
    /// A recorded response for this operation identity: the provider is not
    /// called again, so a retry after an unknown outcome cannot re-execute.
    Replay(Value),
}

fn request(store: &SharedStore, args: &Value, caller: Caller) -> CommandResult {
    // Each guard is scoped to its phase; none is alive during `generate`.
    let phase = {
        let mut guard = lock(store)?;
        prepare(&mut guard, args, caller)?
    };
    let prepared = match phase {
        Phase::Replay(stored) => return Ok(stored),
        Phase::Execute(prepared) => prepared,
    };
    let generated = match generate(&prepared)? {
        Ok(generated) => generated,
        Err(message) => return outage_outcome(message, prepared.fallback),
    };
    let mut guard = lock(store)?;
    record(&mut guard, &prepared, generated)
}

fn prepare(store: &mut Store, args: &Value, caller: Caller) -> Result<Phase, CommandFailure> {
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
    let operation = match args.get("operation_id").and_then(Value::as_str) {
        Some(operation_id) => {
            let request_json = serde_json::to_string(args)
                .map_err(|error| CommandFailure::invalid(error.to_string()))?;
            // Replay BEFORE the provider runs: re-executing on a retry would
            // repeat the external effect (and its cost) for one operation.
            if let Some(stored) = store
                .lookup_operation(
                    &workload.created_by,
                    "inference.request",
                    operation_id,
                    &request_json,
                )
                .map_err(CommandFailure::storage)?
            {
                return Ok(Phase::Replay(
                    serde_json::from_str(&stored)
                        .map_err(|error| CommandFailure::invalid(error.to_string()))?,
                ));
            }
            Some((operation_id.to_owned(), request_json))
        }
        None => None,
    };
    Ok(Phase::Execute(Box::new(Prepared {
        workload,
        provider_id: provider.id,
        fallback,
        socket,
        request_id,
        prompt: prompt.to_owned(),
        max_tokens,
        operation,
    })))
}

/// The provider call, made without the store lock. An outer `Err` is a
/// protocol violation; an inner `Err` is an outage for the fallback policy.
fn generate(prepared: &Prepared) -> Result<Result<Value, String>, CommandFailure> {
    let response = flowerly_core::ipc::call_blocking(
        &prepared.socket,
        Payload::Command {
            name: "provider.generate".to_owned(),
            args: serde_json::json!({
                "request_id": prepared.request_id,
                "prompt": prepared.prompt,
                "max_tokens": prepared.max_tokens,
                "created_by": prepared.workload.created_by,
            }),
        },
    );
    match response {
        Ok(response) => match response.payload {
            Payload::Ok { data } => Ok(Ok(data)),
            Payload::Err { message, .. } => Ok(Err(format!("provider refused: {message}"))),
            other => Err(CommandFailure {
                code: flowerly_core::ipc::ErrorCode::Internal,
                message: format!("unexpected provider response type: {other:?}"),
            }),
        },
        Err(error) => Ok(Err(format!("provider unreachable: {error}"))),
    }
}

/// Ledger the bounded result with unknown-outcome replay. The provider
/// response grants nothing: the only durable artifact is this row.
fn record(store: &mut Store, prepared: &Prepared, generated: Value) -> CommandResult {
    let workload = &prepared.workload;
    let actor = workload.created_by.clone();
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
        "request_id": prepared.request_id,
        "provider": prepared.provider_id.as_str(),
        "executed": true,
        "result": generated,
    });
    let response_value = if let Some((operation_id, request_json)) = &prepared.operation {
        let response_json = serde_json::to_string(&intended)
            .map_err(|error| CommandFailure::invalid(error.to_string()))?;
        let operation = MutationOperation {
            actor: &actor,
            command: "inference.request",
            operation_id,
            request_payload: request_json,
            response_payload: &response_json,
        };
        // Re-checks atomically: a response recorded while the provider ran
        // wins, and this execution's row is not written.
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
                from: ObjectRef::from(&prepared.provider_id),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Mutex};

    use flowerly_core::ipc::{Envelope, ErrorCode};
    use flowerly_core::profile::BackgroundActivity;

    const ALICE: Caller = Caller::new(1001);

    static RUNTIME: OnceLock<tempfile::TempDir> = OnceLock::new();

    fn runtime_dir() -> PathBuf {
        let dir = RUNTIME.get_or_init(|| tempfile::tempdir().expect("runtime dir"));
        init_runtime_dir(dir.path().to_path_buf());
        dir.path().to_path_buf()
    }

    fn call(store: &SharedStore, name: &str, args: Value) -> Envelope {
        let request = Envelope {
            version: 1,
            id: "test".to_owned(),
            payload: Payload::Command {
                name: name.to_owned(),
                args,
            },
        };
        super::super::handle_command(store, &request, ALICE)
    }

    fn ok(response: &Envelope) -> Value {
        match &response.payload {
            Payload::Ok { data } => data.clone(),
            other => panic!("expected ok, got {other:?}"),
        }
    }

    /// A stand-in provider served by the core IPC runner (the reviewed
    /// transport), at `<service>.sock`. Counts generate calls; when `hold`
    /// is given it reports each call on `started` and answers only after a
    /// release.
    fn fake_provider(
        service: &str,
        hold: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
    ) -> Arc<AtomicUsize> {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let hold = hold.map(Mutex::new);
        let handler = move |request: &Envelope| -> Envelope {
            let count = counted.fetch_add(1, Ordering::SeqCst) + 1;
            if let Some(hold) = &hold {
                let hold = hold.lock().unwrap();
                hold.0.send(()).expect("started");
                hold.1.recv().expect("release");
            }
            flowerly_core::ok_envelope(
                request.id.clone(),
                serde_json::json!({
                    "tokens_in": 2, "tokens_out": 3, "latency_ms": 1,
                    "text": format!("call {count}"),
                }),
            )
        };
        let config = flowerly_core::ServiceConfig {
            name: service.to_owned(),
            runtime_dir: runtime_dir(),
            ..flowerly_core::ServiceConfig::default()
        };
        std::thread::spawn(move || {
            tokio::runtime::Runtime::new()
                .expect("runtime")
                .block_on(flowerly_core::run_service(
                    config,
                    Value::Null,
                    Arc::new(handler),
                ))
                .expect("fake provider");
        });
        let socket = runtime_dir().join(format!("{service}.sock"));
        for _ in 0..200 {
            if socket.exists() {
                return calls;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("fake provider did not bind {}", socket.display());
    }

    /// A store with one ADMITTED workload of Alice's and a provider at
    /// `endpoint`; returns the store, workload id and provider id.
    fn admitted(endpoint: &str) -> (SharedStore, String, String) {
        let mut db = Store::open_in_memory().expect("store");
        db.migrate().expect("migrate");
        let store: SharedStore = Arc::new(std::sync::Mutex::new(db));
        let model = ok(&call(
            &store,
            "model.register",
            serde_json::json!({"name": "m", "created_by": "uid:1001"}),
        ));
        let profile = ok(&call(
            &store,
            "profile.create",
            serde_json::json!({"name": "p", "toml": "fallback = 'fail'\n"}),
        ));
        let provider = ok(&call(
            &store,
            "provider.register",
            serde_json::json!({
                "name": "fake", "endpoint": endpoint, "capabilities": ["text"],
                "created_by": "uid:1001",
            }),
        ));
        // Inserted directly: admission measures the host, which a unit test
        // must not depend on.
        let workload = WorkloadRecord {
            id: WorkloadId::new(),
            profile_id: profile["id"].as_str().unwrap().parse().unwrap(),
            profile_name: "p".to_owned(),
            model_id: model["id"].as_str().unwrap().parse().unwrap(),
            background: BackgroundActivity::default(),
            priority: 0,
            state: WorkloadState::Admitted,
            reason: "test".to_owned(),
            ceilings: serde_json::json!({}),
            created_by: "uid:1001".to_owned(),
            created_at: Timestamp::now(),
            owner: ALICE.owner(),
            admitted_at: Some(Timestamp::now()),
            stopped_at: None,
            completed_at: None,
            operation_id: None,
        };
        store
            .lock()
            .unwrap()
            .insert_workload(&workload, &[], None)
            .expect("workload");
        (
            store,
            workload.id.as_str().to_owned(),
            provider["id"].as_str().unwrap().to_owned(),
        )
    }

    fn inference(workload: &str, provider: &str, prompt: &str, operation: Option<&str>) -> Value {
        serde_json::json!({
            "workload_id": workload, "provider_id": provider, "prompt": prompt,
            "request_id": "req-1", "operation_id": operation, "created_by": "uid:1001",
        })
    }

    /// #263 blocker 4: a retried operation replays without a second
    /// provider execution (a repeated external effect would be billed twice).
    #[test]
    fn a_retried_operation_replays_without_calling_the_provider_again() {
        let calls = fake_provider("replay", None);
        let (store, workload, provider) = admitted("replay.sock");
        let args = inference(&workload, &provider, "hello", Some("op-1"));
        let first = ok(&call(&store, "inference.request", args.clone()));
        let second = ok(&call(&store, "inference.request", args));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "replay re-executed");
        assert_eq!(first, second);
        assert_eq!(first["result"]["text"], "call 1");

        // A reused identity with different arguments is a conflict, and
        // still never reaches the provider.
        let reused = call(
            &store,
            "inference.request",
            inference(&workload, &provider, "other", Some("op-1")),
        );
        assert!(matches!(
            reused.payload,
            Payload::Err {
                code: Some(ErrorCode::Conflict),
                ..
            }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Without an operation identity every request executes.
        ok(&call(
            &store,
            "inference.request",
            inference(&workload, &provider, "hello", None),
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// #263 blocker 2: the shared store stays usable while a provider call
    /// is in flight.
    #[test]
    fn the_store_lock_is_not_held_during_the_provider_call() {
        let (started_tx, started) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let calls = fake_provider("held", Some((started_tx, release_rx)));
        let (store, workload, provider) = admitted("held.sock");
        let in_flight = {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                call(
                    &store,
                    "inference.request",
                    inference(&workload, &provider, "hello", Some("op-held")),
                )
            })
        };
        started
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("provider reached");
        assert!(
            store.try_lock().is_ok(),
            "store locked across the provider call"
        );
        ok(&call(&store, "model.list", serde_json::json!({})));
        release.send(()).expect("release");
        let response = in_flight.join().expect("request thread");
        assert_eq!(ok(&response)["executed"], true);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
