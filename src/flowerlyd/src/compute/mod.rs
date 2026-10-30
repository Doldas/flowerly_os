//! Local model catalog, measured device budget and honest compatibility
//! checks for RFC 0004's inference placement exploration.
//!
//! Boundary invariants (RFC 0004 foundational invariants):
//! - The catalog stores measured evidence only; unknown values stay unknown
//!   and are never silently estimated or substituted.
//! - A catalog entry, budget verdict or profile can never grant OS authority;
//!   this module performs no process, filesystem or network actions.
//! - No provider, credential, endpoint or egress configuration lives here.
mod host;

use host::{HostTelemetry, LinuxHostTelemetry};
use std::str::FromStr;
use std::sync::Arc;

use flowerly_core::ipc::{Envelope, ErrorCode, Payload};
use flowerly_core::storage::{MutationOperation, StorageError, Store};
use flowerly_core::types::Caller;
use flowerly_core::types::{
    ModelBackend, ModelId, ModelRecord, ObjectRef, ProvenanceEdge, ProvenanceRelation, Timestamp,
};
use flowerly_core::{coded_error_envelope, error_envelope, ok_envelope};
use serde_json::Value;

pub(crate) type SharedStore = Arc<std::sync::Mutex<Store>>;

pub(crate) fn handle_command(store: &SharedStore, request: &Envelope, caller: Caller) -> Envelope {
    let Payload::Command { name, args } = &request.payload else {
        return error_envelope(request.id.clone(), "unexpected request type");
    };
    // Inference manages the store lock itself: it must not be held across
    // a provider call, which every other module's requests would wait behind.
    if name.starts_with("inference.") {
        return match inference::run_inference(store, name, args, caller) {
            Ok(data) => ok_envelope(request.id.clone(), data),
            Err(error) => coded_error_envelope(request.id.clone(), error.code, error.message),
        };
    }
    let mut store = match store.lock() {
        Ok(store) => store,
        Err(_) => {
            return error_envelope(request.id.clone(), "storage lock poisoned");
        }
    };
    let data = match run_command(&mut store, name, args, caller) {
        Ok(data) => data,
        Err(error) => {
            return coded_error_envelope(request.id.clone(), error.code, error.message);
        }
    };
    ok_envelope(request.id.clone(), data)
}

mod admission;
mod inference;
mod profiles;

pub(crate) use inference::init_runtime_dir;

#[derive(Debug)]
pub(crate) struct CommandFailure {
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

    fn storage(error: StorageError) -> Self {
        Self {
            code: error.ipc_error_code(),
            message: error.to_string(),
        }
    }
}

impl From<&str> for CommandFailure {
    fn from(message: &str) -> Self {
        Self::invalid(message)
    }
}

pub(crate) type CommandResult = Result<Value, CommandFailure>;

fn parse_u64(args: &Value, field: &str) -> Result<Option<u64>, CommandFailure> {
    let Some(value) = args.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    match value.as_u64() {
        Some(raw) if raw > 0 => Ok(Some(raw)),
        Some(_) => Err(CommandFailure::invalid(format!(
            "`{field}` must be a positive integer"
        ))),
        None => match value.as_f64() {
            Some(_) => Err(CommandFailure::invalid(format!(
                "`{field}` must be an integer, not a float"
            ))),
            None => Err(CommandFailure::invalid(format!(
                "`{field}` must be a non-negative integer"
            ))),
        },
    }
}

fn parse_string(args: &Value, field: &str) -> Result<Option<String>, CommandFailure> {
    let Some(value) = args.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(text) = value.as_str() else {
        return Err(CommandFailure::invalid(format!(
            "`{field}` must be a string or null"
        )));
    };
    if text.trim().is_empty() {
        return Err(CommandFailure::invalid(format!(
            "`{field}` must not be blank"
        )));
    }
    Ok(Some(text.to_owned()))
}

fn parse_modalities(args: &Value) -> Result<Vec<String>, CommandFailure> {
    let Some(value) = args.get("modalities") else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let Some(items) = value.as_array() else {
        return Err(CommandFailure::invalid(
            "`modalities` must be a string array",
        ));
    };
    if items.len() > 16 {
        return Err(CommandFailure::invalid(
            "`modalities` must hold at most 16 entries",
        ));
    }
    let mut modalities = Vec::with_capacity(items.len());
    for item in items {
        let Some(text) = item.as_str() else {
            return Err(CommandFailure::invalid("`modalities` must contain strings"));
        };
        let trimmed = text.trim().to_ascii_lowercase();
        if trimmed.is_empty() || trimmed.len() > 32 {
            return Err(CommandFailure::invalid(
                "`modalities` entries must be 1..32 characters",
            ));
        }
        if !modalities.contains(&trimmed) {
            modalities.push(trimmed);
        }
    }
    Ok(modalities)
}

fn validate_registration(args: &Value) -> Result<(), CommandFailure> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| CommandFailure::invalid("missing string arg `name`"))?;
    if name.trim().is_empty() || name.len() > 256 {
        return Err(CommandFailure::invalid(
            "model name must be 1..256 characters",
        ));
    }
    if let Some(operation_id) = args.get("operation_id") {
        if !operation_id.is_null()
            && (!operation_id.is_string()
                || !operation_id.as_str().is_some_and(idempotency_key_valid))
        {
            return Err(CommandFailure::invalid(
                "operation_id must use 1..128 ASCII letters, numbers, ., _, :, or -",
            ));
        }
    }
    if let Some(Value::String(backend)) = args.get("backend") {
        if ModelBackend::from_str(backend).is_err() {
            return Err(CommandFailure::invalid(format!(
                "unknown model backend `{backend}`"
            )));
        }
    } else if !args.get("backend").is_none_or(Value::is_null) {
        return Err(CommandFailure::invalid(
            "`backend` must be a string or null",
        ));
    }
    for field in ["context_tokens", "measured_memory_bytes"] {
        parse_u64(args, field)?;
    }
    for field in ["family", "architecture", "license", "evidence"] {
        parse_string(args, field)?;
    }
    if args
        .get("license")
        .and_then(Value::as_str)
        .is_some_and(|l| l.len() > 256)
    {
        return Err(CommandFailure::invalid(
            "`license` must be at most 256 characters",
        ));
    }
    if args
        .get("evidence")
        .and_then(Value::as_str)
        .is_some_and(|e| e.len() > 512)
    {
        return Err(CommandFailure::invalid(
            "`evidence` must be at most 512 characters",
        ));
    }
    parse_modalities(args)?;
    Ok(())
}

fn idempotency_key_valid(raw: &str) -> bool {
    raw.len() <= 128
        && !raw.is_empty()
        && raw
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
}

fn run_command(store: &mut Store, name: &str, args: &Value, caller: Caller) -> CommandResult {
    match name {
        "model.register" => validate_registration(args)?,
        "model.list" | "model.show" | "model.retire" | "device.budget" | "model.compatibility" => {}
        _ => {
            if name.starts_with("profile.") {
                return profiles::run_profile(store, name, args, caller);
            }
            if name.starts_with("provider.") {
                return profiles::run_provider(store, name, args);
            }
            if name.starts_with("workload.") || name.starts_with("usage.") {
                return admission::run(store, name, args, caller);
            }
            return Err(CommandFailure::invalid(format!(
                "compute: unknown command `{name}`"
            )));
        }
    }
    run_checked(store, name, args)
}

fn run_checked(store: &mut Store, name: &str, args: &Value) -> CommandResult {
    match name {
        "model.register" => register_model(store, args),
        "model.list" => {
            let models = store.list_models().map_err(CommandFailure::storage)?;
            Ok(serde_json::json!({
                "models": models.iter().map(model_summary).collect::<Vec<_>>(),
            }))
        }
        "model.show" => {
            let id = parse_model_id(args)?;
            let model = store
                .get_model(&id)
                .map_err(CommandFailure::storage)?
                .ok_or_else(|| CommandFailure {
                    code: ErrorCode::NotFound,
                    message: format!("model not found: {id}"),
                })?;
            Ok(model_detail(&model))
        }
        "model.retire" => {
            let id = parse_model_id(args)?;
            let actor = actor_from(args);
            store
                .retire_model(&id, &actor)
                .map_err(CommandFailure::storage)?;
            Ok(serde_json::json!({ "id": id.as_str(), "retired": true }))
        }
        "device.budget" => Ok(DeviceBudget::measure().to_json()),
        "model.compatibility" => {
            let id = parse_model_id(args)?;
            let model = store
                .get_model(&id)
                .map_err(CommandFailure::storage)?
                .ok_or_else(|| CommandFailure {
                    code: ErrorCode::NotFound,
                    message: format!("model not found: {id}"),
                })?;
            Ok(CompatibilityVerdict::for_model(&model).to_json())
        }
        other => Err(CommandFailure::invalid(format!(
            "compute: unknown command `{other}`"
        ))),
    }
}

fn parse_model_id(args: &Value) -> Result<ModelId, CommandFailure> {
    let raw = args
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| CommandFailure::invalid("missing string arg `id`"))?;
    ModelId::from_str(raw)
        .map_err(|error| CommandFailure::invalid(format!("invalid model id: {error}")))
}

pub(crate) fn actor_from(args: &Value) -> String {
    args.get("created_by")
        .and_then(Value::as_str)
        .unwrap_or("user")
        .to_owned()
}

fn register_model(store: &mut Store, args: &Value) -> CommandResult {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .expect("validated registration always holds `name`")
        .to_owned();
    let created_by = actor_from(args);
    let mut model = ModelRecord::new(&name, created_by.clone());
    model.family = parse_string(args, "family")?;
    model.architecture = parse_string(args, "architecture")?;
    model.backend = args
        .get("backend")
        .and_then(Value::as_str)
        .and_then(|raw| ModelBackend::from_str(raw).ok())
        .unwrap_or(ModelBackend::Unknown);
    model.context_tokens = parse_u64(args, "context_tokens")?;
    model.measured_memory_bytes = parse_u64(args, "measured_memory_bytes")?;
    model.modalities = parse_modalities(args)?;
    model.license = parse_string(args, "license")?;
    model.evidence = parse_string(args, "evidence")?;
    let provenance = ProvenanceEdge {
        from: ObjectRef::from(&model.id),
        relation: ProvenanceRelation::CreatedBy,
        to: ObjectRef::actor(&created_by)
            .map_err(|error| CommandFailure::invalid(error.to_string()))?,
        created_at: Timestamp::now(),
        note: None,
    };
    let intended_response = serde_json::to_value(model_summary(&model))
        .map_err(|error| CommandFailure::invalid(error.to_string()))?;
    let response = if let Some(operation_id) = args.get("operation_id").and_then(Value::as_str) {
        let request_json = serde_json::to_string(args)
            .map_err(|error| CommandFailure::invalid(error.to_string()))?;
        let response_json = serde_json::to_string(&intended_response)
            .map_err(|error| CommandFailure::invalid(error.to_string()))?;
        let operation = MutationOperation {
            actor: &created_by,
            command: "model.register",
            operation_id,
            request_payload: &request_json,
            response_payload: &response_json,
        };
        let stored = store
            .insert_model_with_provenance_once(&model, &[provenance], &operation)
            .map_err(CommandFailure::storage)?;
        serde_json::from_str(&stored).map_err(|error| CommandFailure::invalid(error.to_string()))?
    } else {
        store
            .insert_model_with_provenance(&model, &[provenance])
            .map_err(CommandFailure::storage)?;
        intended_response
    };
    tracing::info!(model_id = %model.id, name, "model registered");
    Ok(response)
}

fn model_summary(model: &ModelRecord) -> Value {
    serde_json::json!({
        "id": model.id.as_str(),
        "name": model.name,
        "family": model.family,
        "backend": model.backend.to_string(),
        "modalities": model.modalities,
        "evidence": model.evidence,
    })
}

fn model_detail(model: &ModelRecord) -> Value {
    let mut detail = model_summary(model);
    if let Some(fields) = detail.as_object_mut() {
        fields.insert(
            "context_tokens".into(),
            serde_json::json!(model.context_tokens),
        );
        fields.insert(
            "measured_memory_bytes".into(),
            serde_json::json!(model.measured_memory_bytes),
        );
        fields.insert("license".into(), serde_json::json!(model.license));
        fields.insert("created_by".into(), serde_json::json!(model.created_by));
        fields.insert(
            "created_at".into(),
            serde_json::json!(model.created_at.as_millis()),
        );
        fields.insert(
            "retired_at".into(),
            serde_json::json!(model.retired_at.map(|t| t.as_millis())),
        );
    }
    detail
}

/// Measured device budget. `Linux memory-info adapter` is the only enforced numeric
/// source; every other constraint is labeled honestly as unenforced or
/// unknown instead of inventing numbers (RFC 0004 invariant: report
/// feasibility honestly).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct DeviceBudget {
    pub total_memory_bytes: Option<u64>,
    pub available_memory_bytes: Option<u64>,
    pub gpu_detected: Vec<String>,
    pub meminfo_source_available: bool,
}

impl DeviceBudget {
    fn from_meminfo(text: &str) -> Self {
        let parse_field = |field: &str| -> Option<u64> {
            text.lines()
                .find_map(|line| {
                    let (label, rest) = line.split_once(':')?;
                    if label != field {
                        return None;
                    }
                    rest.split_whitespace()
                        .next()?
                        .parse::<u64>()
                        .ok()
                        .map(|kib| kib.saturating_mul(1024))
                })
                .filter(|value| *value > 0)
        };
        Self {
            total_memory_bytes: parse_field("MemTotal"),
            available_memory_bytes: parse_field("MemAvailable"),
            gpu_detected: detect_gpu_nodes(),
            meminfo_source_available: true,
        }
    }

    pub(crate) fn measure() -> Self {
        Self::measure_with(&LinuxHostTelemetry)
    }

    fn measure_with(host: &impl HostTelemetry) -> Self {
        match host.memory_info() {
            Ok(text) => Self::from_meminfo(&text),
            Err(_) => Self {
                meminfo_source_available: false,
                ..Self::default()
            },
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        serde_json::json!({
            "total_memory_bytes": self.total_memory_bytes,
            "available_memory_bytes": self.available_memory_bytes,
            "gpu_detected": self.gpu_detected,
            "meminfo_source_available": self.meminfo_source_available,
            "enforcement": {
                "cpu_memory": "advisory-measured",
                "gpu_vram": "not-enforced",
                "disk": "not-measured",
            },
        })
    }
}

/// Detect DRM render-node presence without claiming any capability beyond
/// observation. Card names are honest hardware observations only.
fn detect_gpu_nodes() -> Vec<String> {
    LinuxHostTelemetry.drm_cards().unwrap_or_default()
}

/// Compatibility verdict against the measured budget. Missing catalog
/// evidence yields `Unknown` with explicit reasons; nothing is estimated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CompatibilityVerdict {
    Fits {
        available_memory_bytes: u64,
    },
    ExceedsBudget {
        requested_memory_bytes: u64,
        available_memory_bytes: u64,
    },
    Unknown {
        reasons: Vec<String>,
    },
}

impl CompatibilityVerdict {
    pub(crate) fn for_model(model: &ModelRecord) -> Self {
        let mut reasons = Vec::new();
        if model.is_retired() {
            reasons.push("model is retired".to_owned());
        }
        if model.measured_memory_bytes.is_none() {
            reasons.push("no measured memory footprint recorded".to_owned());
        }
        if model.backend == ModelBackend::Unknown {
            reasons.push("execution backend is unrecorded".to_owned());
        }
        if !reasons.is_empty() {
            return Self::Unknown { reasons };
        }
        let budget = DeviceBudget::measure();
        let Some(available) = budget.available_memory_bytes else {
            reasons.push("measured device memory is unavailable".to_owned());
            return Self::Unknown { reasons };
        };
        let requested = model
            .measured_memory_bytes
            .expect("recorded memory verified above");
        if requested > available {
            Self::ExceedsBudget {
                requested_memory_bytes: requested,
                available_memory_bytes: available,
            }
        } else {
            Self::Fits {
                available_memory_bytes: available,
            }
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        match self {
            Self::Fits {
                available_memory_bytes,
            } => serde_json::json!({
                "verdict": "FITS",
                "available_memory_bytes": available_memory_bytes,
                "promise": "advisory-only",
            }),
            Self::ExceedsBudget {
                requested_memory_bytes,
                available_memory_bytes,
            } => serde_json::json!({
                "verdict": "EXCEEDS_BUDGET",
                "requested_memory_bytes": requested_memory_bytes,
                "available_memory_bytes": available_memory_bytes,
                "promise": "advisory-only",
            }),
            Self::Unknown { reasons } => serde_json::json!({
                "verdict": "UNKNOWN",
                "reasons": reasons,
                "promise": "advisory-only",
            }),
        }
    }
}

pub(crate) async fn self_test() -> anyhow::Result<()> {
    let budget = DeviceBudget::measure();
    if budget.total_memory_bytes.is_none() && budget.meminfo_source_available {
        anyhow::bail!("meminfo claims availability without parseable memory totals");
    }
    admission::self_probe().map_err(anyhow::Error::msg)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CALLER: Caller = Caller::new(1000);

    use flowerly_core::ipc::Envelope;

    fn payload(name: &str, args: Value) -> Envelope {
        Envelope {
            version: 1,
            id: "test-envelope-id".to_owned(),
            payload: Payload::Command {
                name: name.to_owned(),
                args,
            },
        }
    }

    fn call(store: &SharedStore, name: &str, args: Value) -> Envelope {
        handle_command(store, &payload(name, args), TEST_CALLER)
    }

    fn store() -> SharedStore {
        let mut db = Store::open_in_memory().expect("open test store");
        db.migrate().expect("migrate test store");
        Arc::new(std::sync::Mutex::new(db))
    }

    fn assert_ok(response: &Envelope) -> Value {
        let Payload::Ok { data } = &response.payload else {
            let Payload::Err { message, code, .. } = &response.payload else {
                panic!("unexpected envelope");
            };
            panic!("expected ok, got error {code:?}: {message}");
        };
        data.clone()
    }

    fn assert_error(response: &Envelope, expected: ErrorCode) -> String {
        let Payload::Err { message, code, .. } = &response.payload else {
            let Payload::Ok { data } = &response.payload else {
                panic!("expected error, got unexpected variant");
            };
            panic!("expected error {expected:?}, got ok: {data:?}");
        };
        assert_eq!(*code, Some(expected), "message: {message}");
        message.clone()
    }

    fn register_extra(store: &SharedStore, extra: Value) -> Value {
        let mut args = serde_json::json!({
            "name": "qwen2-coder-14b",
            "backend": "CPU",
            "measured_memory_bytes": 9_000_000_000u64,
            "context_tokens": 32_768u64,
            "modalities": ["text"],
            "license": "apache-2.0",
            "evidence": "measure:gold-rss",
            "created_by": "uid:1000",
        });
        if let (Value::Object(base), Value::Object(patch)) = (&mut args, extra) {
            base.extend(patch);
        }
        let response = call(store, "model.register", args);
        assert_ok(&response)
    }

    #[test]
    fn meminfo_budget_is_parsed_from_kib() {
        let meminfo = "MemTotal:        16384000 kB\n\
                       MemFree:          1024000 kB\n\
                       MemAvailable:    10240000 kB\n";
        let budget = DeviceBudget::from_meminfo(meminfo);
        assert_eq!(budget.total_memory_bytes, Some(16_777_216_000));
        assert_eq!(budget.available_memory_bytes, Some(10_485_760_000));
        assert!(budget.meminfo_source_available);
    }

    #[test]
    fn meminfo_zeroes_and_missing_fields_stay_none() {
        let budget = DeviceBudget::from_meminfo("MemTotal: 0 kB\n");
        assert_eq!(budget.total_memory_bytes, None);
        assert!(budget.meminfo_source_available);
    }

    #[test]
    fn compatibility_reports_unknown_for_missing_evidence() {
        let mut model = ModelRecord::new("unmeasured", "uid:1000");
        model.backend = ModelBackend::Cpu;
        let verdict = CompatibilityVerdict::for_model(&model);
        let CompatibilityVerdict::Unknown { reasons } = verdict else {
            panic!("unmeasured model must be unknown");
        };
        assert!(reasons
            .iter()
            .any(|reason| reason.contains("measured memory")));
    }

    #[test]
    fn compatibility_reports_exceeds_budget_deterministically() {
        let mut model = ModelRecord::new("huge-model", "uid:1000");
        model.backend = ModelBackend::Cpu;
        model.measured_memory_bytes = Some(u64::MAX);
        let json = CompatibilityVerdict::for_model(&model).to_json();
        assert_eq!(json["verdict"], "EXCEEDS_BUDGET");
        assert_eq!(json["requested_memory_bytes"], u64::MAX);
    }

    #[test]
    fn register_validates_all_metadata_strictly() {
        let store = store();
        for (reason, args) in [
            ("blank name", serde_json::json!({"name": " "})),
            (
                "bad backend",
                serde_json::json!({"name": "m", "backend": "MAGIC"}),
            ),
            (
                "float memory",
                serde_json::json!({"name": "m", "measured_memory_bytes": 1.5}),
            ),
            (
                "wrong op key",
                serde_json::json!({"name": "m", "operation_id": "bad key"}),
            ),
            (
                "modalities not strings",
                serde_json::json!({"name": "m", "modalities": [1]}),
            ),
        ] {
            let mut patch = serde_json::json!({"created_by": "uid:1000", "_reason": reason});
            if let (Value::Object(base), Value::Object(fields)) = (&mut patch, args) {
                base.extend(fields.clone());
            }
            assert_error(
                &call(&store, "model.register", patch),
                ErrorCode::InvalidArgument,
            );
        }
        assert!(store.lock().unwrap().list_models().unwrap().is_empty());
    }

    #[test]
    fn register_list_show_compatibility_retire_round_trip() {
        let store = store();
        let data = register_extra(&store, serde_json::json!({}));
        let id = data["id"].as_str().expect("registered id").to_owned();
        let list = assert_ok(&call(&store, "model.list", serde_json::json!({})));
        assert_eq!(list["models"].as_array().map(Vec::len), Some(1));
        let show = assert_ok(&call(&store, "model.show", serde_json::json!({"id": id})));
        assert_eq!(
            show["measured_memory_bytes"],
            serde_json::json!(9_000_000_000u64)
        );
        let compat = assert_ok(&call(
            &store,
            "model.compatibility",
            serde_json::json!({"id": id}),
        ));
        let verdict = compat["verdict"].as_str().expect("verdict string");
        assert!(
            matches!(verdict, "FITS" | "EXCEEDS_BUDGET"),
            "honest verdict: {verdict}"
        );
        assert_eq!(compat["promise"], "advisory-only");
        assert_ok(&call(&store, "model.retire", serde_json::json!({"id": id})));
        let list = assert_ok(&call(&store, "model.list", serde_json::json!({})));
        assert!(list["models"].as_array().expect("models array").is_empty());
        // Retired rows keep full retrievability through model.show.
        let show = assert_ok(&call(&store, "model.show", serde_json::json!({"id": id})));
        assert!(show["retired_at"].as_i64().is_some());
    }

    #[test]
    fn replay_with_same_operation_id_returns_same_runtime_id() {
        let store = store();
        let args = serde_json::json!({
            "name": "replayable",
            "created_by": "uid:1000",
            "operation_id": "op-replay-1",
            "measured_memory_bytes": 1_000_000u64,
        });
        let first = call(&store, "model.register", args.clone());
        let second = call(&store, "model.register", args);
        let first_id = assert_ok(&first)["id"].as_str().expect("id").to_owned();
        let second_id = assert_ok(&second)["id"].as_str().expect("id").to_owned();
        assert_eq!(first_id, second_id);
        assert_eq!(store.lock().unwrap().list_models().unwrap().len(), 1);
    }

    #[test]
    fn device_budget_reports_enforcement_honestly() {
        let store = store();
        let data = assert_ok(&call(&store, "device.budget", serde_json::json!({})));
        let meminfo_available = data["meminfo_source_available"].as_bool().expect("flag");
        assert_eq!(meminfo_available, LinuxHostTelemetry.memory_info().is_ok());
        assert_eq!(data["enforcement"]["gpu_vram"], "not-enforced");
        assert_eq!(data["enforcement"]["cpu_memory"], "advisory-measured");
    }

    #[test]
    fn show_and_compatibility_and_retire_reject_unknown_ids() {
        let store = store();
        for name in ["model.show", "model.compatibility", "model.retire"] {
            let response = call(
                &store,
                name,
                serde_json::json!({"id": format!("model-{}", uuid::Uuid::new_v4().simple())}),
            );
            let message = assert_error(&response, ErrorCode::NotFound);
            assert!(message.contains("model not found"), "{message}");
        }
    }

    const PROFILE_TOML: &str = "capability_tier = 'advanced'\nbackground_activity = 'quiet'\n";

    fn create_profile(store: &SharedStore, name: &str) -> String {
        let data = assert_ok(&call(
            store,
            "profile.create",
            serde_json::json!({"name": name, "toml": PROFILE_TOML, "created_by": "uid:1000"}),
        ));
        data["id"].as_str().expect("profile id").to_owned()
    }

    /// #53/#63: a retried clone with the same key must replay the first
    /// clone, not report a freshly generated id that was never stored.
    #[test]
    fn profile_clone_replay_returns_the_stored_clone() {
        let store = store();
        let source = create_profile(&store, "source");
        let args = serde_json::json!({
            "id": source, "name": "copy", "operation_id": "clone-1", "created_by": "uid:1000"
        });
        let first = assert_ok(&call(&store, "profile.clone", args.clone()));
        let second = assert_ok(&call(&store, "profile.clone", args));
        assert_eq!(first["id"], second["id"]);
        let shown = call(
            &store,
            "profile.show",
            serde_json::json!({"id": second["id"]}),
        );
        assert_eq!(assert_ok(&shown)["name"], "copy");
        assert_eq!(
            store
                .lock()
                .unwrap()
                .profiles_for(TEST_CALLER)
                .unwrap()
                .len(),
            2
        );
        let reused = call(
            &store,
            "profile.clone",
            serde_json::json!({
                "id": source, "name": "other", "operation_id": "clone-1", "created_by": "uid:1000"
            }),
        );
        assert_error(&reused, ErrorCode::Conflict);
    }

    /// #53/#63: `operation_id` on profile.edit was validated and dropped, so
    /// a retry appended a duplicate version.
    #[test]
    fn profile_edit_replay_does_not_append_a_second_version() {
        let store = store();
        let id = create_profile(&store, "edited");
        let edit = serde_json::json!({
            "id": id,
            "toml": "capability_tier = 'advanced'\nbackground_activity = 'normal'\n",
            "operation_id": "edit-1",
            "created_by": "uid:1000",
        });
        let first = assert_ok(&call(&store, "profile.edit", edit.clone()));
        let second = assert_ok(&call(&store, "profile.edit", edit));
        assert_eq!(first, serde_json::json!({"id": id, "version": 2}));
        assert_eq!(second, first);
        let history = assert_ok(&call(
            &store,
            "profile.version_history",
            serde_json::json!({"id": id}),
        ));
        assert_eq!(history["versions"].as_array().map(Vec::len), Some(2));
        let reused = call(
            &store,
            "profile.edit",
            serde_json::json!({
                "id": id, "toml": "capability_tier = 'advanced'\nbackground_activity = 'intensive'\n",
                "operation_id": "edit-1", "created_by": "uid:1000"
            }),
        );
        assert_error(&reused, ErrorCode::Conflict);
    }

    #[test]
    fn profile_rename_rejects_a_malformed_operation_id() {
        let store = store();
        let id = create_profile(&store, "named");
        for bad in [serde_json::json!(7), serde_json::json!("bad key")] {
            let reply = call(
                &store,
                "profile.rename",
                serde_json::json!({"id": id, "name": "renamed", "operation_id": bad}),
            );
            assert_error(&reply, ErrorCode::InvalidArgument);
        }
        let shown = assert_ok(&call(&store, "profile.show", serde_json::json!({"id": id})));
        assert_eq!(shown["name"], "named");
    }

    /// #53/#63: a retried registration must replay the stored provider id.
    #[test]
    fn provider_register_replay_returns_the_stored_provider() {
        let store = store();
        let args = serde_json::json!({
            "name": "local", "capabilities": ["text"], "operation_id": "prov-1",
            "created_by": "uid:1000",
        });
        let first = assert_ok(&call(&store, "provider.register", args.clone()));
        let second = assert_ok(&call(&store, "provider.register", args));
        assert_eq!(first["id"], second["id"]);
        assert_ok(&call(
            &store,
            "provider.show",
            serde_json::json!({"id": second["id"]}),
        ));
        assert_eq!(store.lock().unwrap().list_providers().unwrap().len(), 1);
        let reused = call(
            &store,
            "provider.register",
            serde_json::json!({
                "name": "local", "capabilities": ["image"], "operation_id": "prov-1",
                "created_by": "uid:1000",
            }),
        );
        assert_error(&reused, ErrorCode::Conflict);
    }

    #[test]
    fn usage_record_refuses_an_operation_id_it_cannot_honor() {
        let store = store();
        let reply = call(
            &store,
            "usage.record",
            serde_json::json!({
                "workload_id": format!("wl-{}", uuid::Uuid::new_v4().simple()),
                "tokens_in": 1, "tokens_out": 1, "latency_ms": 1, "cost_micros": 0,
                "operation_id": "usage-1",
            }),
        );
        let message = assert_error(&reply, ErrorCode::InvalidArgument);
        assert!(message.contains("operation_id"), "{message}");
    }

    #[test]
    fn workload_list_treats_null_mine_as_absent() {
        let store = store();
        for mine in [serde_json::json!(null), serde_json::json!(true)] {
            assert_ok(&call(
                &store,
                "workload.list",
                serde_json::json!({"mine": mine, "created_by": "uid:1000"}),
            ));
        }
        // Listing everyone's workloads is no longer a User command (#225).
        for mine in [serde_json::json!(false), serde_json::json!("yes")] {
            assert_error(
                &call(&store, "workload.list", serde_json::json!({"mine": mine})),
                ErrorCode::InvalidArgument,
            );
        }
    }

    const ALICE: Caller = Caller::new(1001);
    const BOB: Caller = Caller::new(1002);

    fn call_as(store: &SharedStore, caller: Caller, name: &str, args: Value) -> Envelope {
        handle_command(store, &payload(name, args), caller)
    }

    fn profile_as(store: &SharedStore, caller: Caller, name: &str, toml: &str) -> String {
        let created = assert_ok(&call_as(
            store,
            caller,
            "profile.create",
            serde_json::json!({ "name": name, "toml": toml }),
        ));
        created["id"].as_str().unwrap().to_owned()
    }

    fn system_profile(store: &SharedStore, name: &str) -> String {
        let id = flowerly_core::types::ProfileId::new();
        store
            .lock()
            .unwrap()
            .create_profile(
                &id,
                name,
                "system",
                "fallback = 'ask'\n",
                "{}",
                flowerly_core::types::Owner::System,
                None,
                &[],
            )
            .unwrap();
        id.as_str().to_owned()
    }

    fn workload_as(store: &SharedStore, caller: Caller, profile: &str, model: &str) -> String {
        let submitted = assert_ok(&call_as(
            store,
            caller,
            "workload.submit",
            serde_json::json!({ "profile_id": profile, "model_id": model }),
        ));
        submitted["id"].as_str().unwrap().to_owned()
    }

    #[test]
    fn profiles_are_private_to_their_owner_and_names_are_per_owner() {
        let store = store();
        let alice = profile_as(&store, ALICE, "expert", "fallback = 'ask'\n");
        // The same name is free for another owner: no existence oracle.
        let bob = profile_as(&store, BOB, "expert", "fallback = 'queue'\n");
        let clash = call_as(
            &store,
            ALICE,
            "profile.create",
            serde_json::json!({ "name": "expert", "toml": "fallback = 'ask'\n" }),
        );
        assert_error(&clash, ErrorCode::Conflict);

        let listed = assert_ok(&call_as(&store, BOB, "profile.list", serde_json::json!({})));
        let ids: Vec<&str> = listed["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, [bob.as_str()]);

        let missing = format!("profile-{}", "0".repeat(32));
        for (name, extra) in [
            ("profile.show", serde_json::json!({})),
            ("profile.export", serde_json::json!({})),
            ("profile.version_history", serde_json::json!({})),
            ("profile.resolve", serde_json::json!({})),
            ("profile.clone", serde_json::json!({ "name": "copy" })),
            ("profile.rename", serde_json::json!({ "name": "mine-now" })),
            (
                "profile.edit",
                serde_json::json!({ "toml": "fallback = 'fail'\n" }),
            ),
        ] {
            let mut args = extra.clone();
            args["id"] = serde_json::json!(alice);
            let foreign = assert_error(&call_as(&store, BOB, name, args), ErrorCode::NotFound);
            let mut args = extra;
            args["id"] = serde_json::json!(missing);
            let absent = call_as(&store, BOB, name, args);
            if let Payload::Err { code, .. } = &absent.payload {
                assert_eq!(*code, Some(ErrorCode::NotFound), "{name}");
            }
            assert!(!foreign.contains("expert"), "{name}: {foreign}");
        }
        let shown = assert_ok(&call_as(
            &store,
            ALICE,
            "profile.show",
            serde_json::json!({ "id": alice }),
        ));
        assert_eq!(shown["name"], "expert");
        assert_eq!(shown["toml"], "fallback = 'ask'\n");
    }

    #[test]
    fn system_profiles_are_shared_read_only_and_resolve_only_system_bases() {
        let store = store();
        let shared = system_profile(&store, "baseline");
        for caller in [ALICE, BOB] {
            let shown = assert_ok(&call_as(
                &store,
                caller,
                "profile.show",
                serde_json::json!({ "id": shared }),
            ));
            assert_eq!(shown["owner"], "system");
            let edit = call_as(
                &store,
                caller,
                "profile.edit",
                serde_json::json!({ "id": shared, "toml": "fallback = 'fail'\n" }),
            );
            assert_error(&edit, ErrorCode::PermissionDenied);
            let rename = call_as(
                &store,
                caller,
                "profile.rename",
                serde_json::json!({ "id": shared, "name": "taken" }),
            );
            assert_error(&rename, ErrorCode::PermissionDenied);
        }
        // Alice's own profile may build on the shared one and may clone it.
        let child = profile_as(
            &store,
            ALICE,
            "derived",
            "base = 'baseline'\nfallback = 'queue'\n",
        );
        let resolved = assert_ok(&call_as(
            &store,
            ALICE,
            "profile.resolve",
            serde_json::json!({ "id": child }),
        ));
        assert_eq!(
            resolved["chain"],
            serde_json::json!(["derived", "baseline"])
        );
        assert_ok(&call_as(
            &store,
            ALICE,
            "profile.clone",
            serde_json::json!({ "id": shared, "name": "my-baseline" }),
        ));
        // Bob cannot name Alice's private profile as a base.
        let foreign_base = call_as(
            &store,
            BOB,
            "profile.create",
            serde_json::json!({ "name": "sneaky", "toml": "base = 'derived'\n" }),
        );
        assert_error(&foreign_base, ErrorCode::NotFound);
    }

    #[test]
    fn workloads_and_usage_are_private_to_their_owner() {
        let store = store();
        let model = register_extra(&store, serde_json::json!({}))["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let alice_profile = profile_as(&store, ALICE, "worker", "fallback = 'queue'\n");
        let shared = system_profile(&store, "shared-worker");
        let alice = workload_as(&store, ALICE, &alice_profile, &model);
        let alice_shared = workload_as(&store, ALICE, &shared, &model);
        // Bob may use the system profile but not Alice's private one.
        let bob = workload_as(&store, BOB, &shared, &model);
        let foreign_profile = call_as(
            &store,
            BOB,
            "workload.submit",
            serde_json::json!({ "profile_id": alice_profile, "model_id": model }),
        );
        assert_error(&foreign_profile, ErrorCode::NotFound);

        // Usage recorded (Admin path) for Alice's workload stays Alice's.
        assert_ok(&call(
            &store,
            "usage.record",
            serde_json::json!({
                "workload_id": alice,
                "tokens_in": 7, "tokens_out": 11, "latency_ms": 3, "cost_micros": 5,
            }),
        ));

        let listed = assert_ok(&call_as(
            &store,
            BOB,
            "workload.list",
            serde_json::json!({}),
        ));
        let ids: Vec<&str> = listed["workloads"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, [bob.as_str()]);
        let listed = assert_ok(&call_as(
            &store,
            ALICE,
            "workload.list",
            serde_json::json!({}),
        ));
        assert_eq!(listed["count"], 2);

        for name in ["workload.show", "workload.stop", "workload.complete"] {
            for id in [&alice, &alice_shared] {
                let reply = call_as(&store, BOB, name, serde_json::json!({ "id": id }));
                assert_error(&reply, ErrorCode::NotFound);
            }
        }
        let foreign_inference = call_as(
            &store,
            BOB,
            "inference.request",
            serde_json::json!({
                "workload_id": alice, "prompt": "hi", "request_id": "r-1",
                "provider_id": format!("provider-{}", "0".repeat(32)),
            }),
        );
        let message = assert_error(&foreign_inference, ErrorCode::NotFound);
        assert!(message.contains("workload not found"), "{message}");

        let bob_brief = assert_ok(&call_as(
            &store,
            BOB,
            "workload.usage_brief",
            serde_json::json!({ "model_id": model }),
        ));
        assert_eq!(bob_brief["tokens_in"], 0);
        let bob_show = assert_ok(&call_as(
            &store,
            BOB,
            "workload.show",
            serde_json::json!({ "id": bob }),
        ));
        assert_eq!(bob_show["usage_tokens_in_total_for_model"], 0);
        let alice_brief = assert_ok(&call_as(
            &store,
            ALICE,
            "workload.usage_brief",
            serde_json::json!({ "model_id": model }),
        ));
        assert_eq!(alice_brief["tokens_in"], 7);
        assert_eq!(alice_brief["cost_micros"], 5);
    }

    #[test]
    fn unknown_compute_commands_are_typed_rejections() {
        let store = store();
        for name in ["model.delete", "device.shutdown", "provider.connect"] {
            assert_error(
                &call(&store, name, serde_json::json!({})),
                ErrorCode::InvalidArgument,
            );
        }
    }
}
