//! Local AI workload admission and usage accounting (RFC 0004 tranche 3).
//!
//! Admission rules are fail-closed and honest:
//! - The referenced catalog model must carry a **measured** memory footprint;
//!   missing evidence is a REFUSED decision with reasons, never a guess.
//! - The resolved profile ceilings apply (memory, concurrency); a refused
//!   decision is recorded with its reasons so accounting stays auditable.
//! - Linux memory pressure (PSI, `Linux PSI adapter`) above a fixed hold
//!   threshold refuses new admission; pressure values are captured in the
//!   decision row.
//! - GPU/accelerator backends are **refused** in this tranche because no
//!   verified VRAM measurement or enforcement exists (RFC 0004 invariant).
//! - No execution happens here: admission is bookkeeping only; no process,
//!   cgroup or network is touched. Ceilings are preference data, never OS
//!   authority.
use super::host::HostTelemetry;
use std::str::FromStr;

use flowerly_core::ipc::ErrorCode;
use flowerly_core::profile::BackgroundActivity;
use flowerly_core::storage::{MutationOperation, Store};
use flowerly_core::types::Caller;
use flowerly_core::types::{
    ModelBackend, ModelId, ObjectRef, ProfileId, ProvenanceEdge, ProvenanceRelation, Timestamp,
    UsageId, WorkloadId, WorkloadRecord, WorkloadState,
};
use serde_json::Value;

use super::{actor_from, profiles, CommandFailure, CommandResult, DeviceBudget};

/// Memory pressure (PSI `avg10`, percent) above which admission refuses.
/// Deliberately high enough to only trigger under real pressure; the value
/// is a guard, not a scheduling guarantee.
const PRESSURE_HOLD_PCT: f64 = 50.0;

fn parse_operation_id(args: &Value) -> Result<Option<String>, CommandFailure> {
    match args.get("operation_id") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(raw))
            if !raw.is_empty()
                && raw.len() <= 128
                && raw
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte)) =>
        {
            Ok(Some(raw.to_owned()))
        }
        Some(other) => Err(CommandFailure::invalid(format!(
            "operation_id must follow the idempotency key rules: {other:?}"
        ))),
    }
}

pub(crate) fn run(store: &mut Store, name: &str, args: &Value, caller: Caller) -> CommandResult {
    match name {
        "workload.submit"
        | "workload.list"
        | "workload.show"
        | "workload.stop"
        | "workload.complete"
        | "workload.usage_brief" => {}
        "usage.record" => validate_usage(args)?,
        other => {
            return Err(CommandFailure::invalid(format!(
                "compute: unknown command `{other}`"
            )));
        }
    }
    match name {
        "workload.submit" => submit(store, args, caller),
        "workload.list" => list(store, args, caller),
        "workload.show" => show(store, args, caller),
        "workload.stop" | "workload.complete" => transition(store, name, args, caller),
        "usage.record" => record_usage(store, args),
        "workload.usage_brief" => usage_brief(store, args, caller),
        other => Err(CommandFailure::invalid(format!(
            "compute: unknown command `{other}`"
        ))),
    }
}

fn validate_usage(args: &Value) -> Result<(), CommandFailure> {
    // Ledger rows are not deduplicated, so a key would promise retry safety
    // that does not exist. Refuse it instead of silently ignoring it.
    if !args.get("operation_id").is_none_or(Value::is_null) {
        return Err(CommandFailure::invalid(
            "usage.record does not support `operation_id`: ledger writes are not deduplicated",
        ));
    }
    for field in ["tokens_in", "tokens_out", "latency_ms", "cost_micros"] {
        match args.get(field) {
            None | Some(Value::Null) => {
                return Err(CommandFailure::invalid(format!(
                    "missing integer arg `{field}`"
                )))
            }
            Some(value) => {
                let valid = value
                    .as_u64()
                    .is_some_and(|raw| raw <= u64::from(u32::MAX) * 4);
                if !valid {
                    return Err(CommandFailure::invalid(format!(
                        "`{field}` must be a bounded non-negative integer"
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Linux PSI summary for the memory controller. Unknown or missing PSI is
/// reported as `None` rather than 0 (fail-closed honesty).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct MemoryPressure {
    pub avg10_pct: Option<f64>,
}

impl MemoryPressure {
    fn from_text(text: &str) -> Option<Self> {
        let line = text.lines().find_map(|line| {
            let (left, _) = line.split_once(' ')?;
            (left == "full").then_some(line)
        })?;
        let avg10 = line.split_whitespace().find_map(|entry| {
            let (key, value) = entry.split_once('=')?;
            (key == "avg10")
                .then(|| value.parse::<f64>().ok())
                .flatten()
        })?;
        Some(Self {
            avg10_pct: Some(avg10),
        })
    }

    pub(crate) fn measure() -> Self {
        Self::measure_with(&super::host::LinuxHostTelemetry)
    }

    pub(super) fn measure_with(host: &impl HostTelemetry) -> Self {
        host.memory_pressure()
            .ok()
            .and_then(|text| Self::from_text(&text))
            .unwrap_or_default()
    }
}

/// Advisory admission guard: refuse when measured free host RAM is below the
/// catalog footprint. This is not reservation, cgroup enforcement or OOM
/// recovery; unknown measurements remain unknown and are not zero.
fn memory_headroom_reason(requested: Option<u64>, available: Option<u64>) -> Option<String> {
    match (requested, available) {
        (Some(required), Some(free)) if required > free => Some(format!(
            "model measured memory {required} exceeds currently available host RAM {free}"
        )),
        _ => None,
    }
}

fn accelerator_backend(backend: ModelBackend) -> bool {
    matches!(
        backend,
        ModelBackend::Cuda | ModelBackend::Vulkan | ModelBackend::Rocm | ModelBackend::Npu
    )
}

fn submit(store: &mut Store, args: &Value, caller: Caller) -> CommandResult {
    let actor = actor_from(args);
    let profile_raw = args
        .get("profile_id")
        .and_then(Value::as_str)
        .ok_or_else(|| CommandFailure::invalid("missing string arg `profile_id`"))?;
    let profile_id: ProfileId = ProfileId::from_str(profile_raw)
        .map_err(|error| CommandFailure::invalid(format!("invalid profile id: {error}")))?;
    let model_raw = args
        .get("model_id")
        .and_then(Value::as_str)
        .ok_or_else(|| CommandFailure::invalid("missing string arg `model_id`"))?;
    let model_id: ModelId = ModelId::from_str(model_raw)
        .map_err(|error| CommandFailure::invalid(format!("invalid model id: {error}")))?;
    let background = match args.get("background") {
        None | Some(Value::Null) => BackgroundActivity::default(),
        Some(Value::String(raw)) => BackgroundActivity::from_str(raw).map_err(|error| {
            CommandFailure::invalid(format!(
                "`background` must be quiet|normal|intensive: {error}"
            ))
        })?,
        Some(other) => {
            return Err(CommandFailure::invalid(format!(
                "`background` must be a string: {other:?}"
            )));
        }
    };
    let priority = match args.get("priority") {
        None | Some(Value::Null) => 0i32,
        Some(value) => value
            .as_i64()
            .filter(|raw| i32::MIN as i64 <= *raw && *raw <= i32::MAX as i64)
            .map(|raw| raw as i32)
            .ok_or_else(|| CommandFailure::invalid("`priority` must be an i32"))?,
    };
    parse_operation_id(args)?;
    let (profile_name, spec) = profiles::resolve_effective(store, caller, &profile_id)?;
    let model = store
        .get_model(&model_id)
        .map_err(CommandFailure::storage)?
        .ok_or_else(|| CommandFailure {
            code: ErrorCode::NotFound,
            message: format!("model not found: {model_id}"),
        })?;

    let mut reasons: Vec<String> = Vec::new();
    if model.is_retired() {
        reasons.push("model is retired".to_owned());
    }
    if let Some(requested) = model.measured_memory_bytes {
        if !matches!(model.backend, ModelBackend::Unknown | ModelBackend::Cpu) {
            reasons.push(format!(
                "backend {} requires accelerator VRAM; no verified VRAM measurement or enforcement exists in this tranche",
                model.backend
            ));
        } else if spec
            .memory_max_bytes
            .is_some_and(|ceiling| ceiling < requested)
        {
            reasons.push(format!(
                "model measured memory {requested} exceeds profile ceiling {}",
                spec.memory_max_bytes.expect("checked above"),
            ));
        }
    } else {
        reasons
            .push("model has no measured memory footprint; refusing without evidence".to_owned());
    }
    // The Linux telemetry facade observes MemAvailable, not a guaranteed
    // reservation. Prevent an obviously over-budget admission while keeping
    // the existing distinction between unknown evidence and measured zero.
    let budget = DeviceBudget::measure();
    if let Some(reason) =
        memory_headroom_reason(model.measured_memory_bytes, budget.available_memory_bytes)
    {
        reasons.push(reason);
    }
    let pressure = MemoryPressure::measure();
    if let Some(avg10) = pressure.avg10_pct {
        if avg10 > PRESSURE_HOLD_PCT {
            reasons.push(format!(
                "memory pressure avg10 {avg10:.1}% exceeds hold threshold {PRESSURE_HOLD_PCT}%"
            ));
        }
    }

    let active = store
        .count_active_workloads_for(caller, &profile_id)
        .map_err(CommandFailure::storage)?;
    let concurrency_limit = spec.concurrency_max.unwrap_or(u32::MAX);
    let state = if reasons.is_empty() && active >= concurrency_limit {
        reasons.push(format!(
            "profile concurrency ceiling reached ({active} active >= {concurrency_limit})"
        ));
        if matches!(
            spec.fallback,
            flowerly_core::profile::FallbackBehavior::Fail
        ) {
            WorkloadState::Refused
        } else {
            WorkloadState::Queued
        }
    } else if reasons.is_empty() {
        WorkloadState::Admitted
    } else {
        WorkloadState::Refused
    };
    let mut reason = if reasons.is_empty() {
        "within the recorded profile ceilings and measured evidence and device pressure".to_owned()
    } else {
        reasons.join("; ")
    };
    if reason.is_empty() {
        reason = "no recorded preconditions".to_owned();
    }
    let ceilings = serde_json::json!({
        "memory_max_bytes": spec.memory_max_bytes,
        "vram_max_bytes": spec.vram_max_bytes,
        "concurrency_max": spec.concurrency_max,
        "locality": spec.locality,
        "capability_tier": spec.capability_tier.as_str(),
        "background_activity": spec.background_activity.as_str(),
    });
    let workload = WorkloadRecord {
        id: WorkloadId::new(),
        profile_id: profile_id.clone(),
        profile_name,
        model_id: model_id.clone(),
        background,
        priority,
        state,
        reason: reason.clone(),
        ceilings: ceilings.clone(),
        created_by: actor.clone(),
        created_at: Timestamp::now(),
        owner: caller.owner(),
        admitted_at: if state == WorkloadState::Admitted {
            Some(Timestamp::now())
        } else {
            None
        },
        stopped_at: None,
        completed_at: None,
        operation_id: args
            .get("operation_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
    };
    let edges = vec![ProvenanceEdge {
        from: ObjectRef::from(&workload.id),
        relation: ProvenanceRelation::CausedBy,
        to: ObjectRef::actor(&actor).map_err(|error| CommandFailure::invalid(error.to_string()))?,
        created_at: Timestamp::now(),
        note: Some("workload admission decision".to_owned()),
    }];
    let intended = serde_json::json!({
        "id": workload.id.as_str(),
        "state": workload.state.to_string(),
        "reason": workload.reason,
        "profile": profile_id.as_str(),
        "model": model_id.as_str(),
        "activity": workload.background.as_str(),
        "priority": workload.priority,
    });
    let response = if let Some(operation_id) = args.get("operation_id").and_then(Value::as_str) {
        let request_json = serde_json::to_string(args)
            .map_err(|error| CommandFailure::invalid(error.to_string()))?;
        let response_json = serde_json::to_string(&intended)
            .map_err(|error| CommandFailure::invalid(error.to_string()))?;
        let operation = MutationOperation {
            actor: &actor,
            command: "workload.submit",
            operation_id,
            request_payload: &request_json,
            response_payload: &response_json,
        };
        let stored = store
            .insert_workload(&workload, &edges, Some(operation))
            .map_err(CommandFailure::storage)?;
        if stored.is_empty() {
            intended
        } else {
            serde_json::from_str(&stored)
                .map_err(|error| CommandFailure::invalid(error.to_string()))?
        }
    } else {
        store
            .insert_workload(&workload, &edges, None)
            .map_err(CommandFailure::storage)?;
        intended
    };
    Ok(response)
}

fn workload_summary(record: &WorkloadRecord) -> Value {
    serde_json::json!({
        "id": record.id.as_str(),
        "profile": record.profile_name,
        "model": record.model_id.as_str(),
        "state": record.state.to_string(),
        "reason": record.reason,
        "background": record.background.as_str(),
        "priority": record.priority,
        "created_at": record.created_at.as_millis(),
    })
}

/// Only the caller's own workloads are listed (#225). `mine: false` used to
/// list everyone's; it is refused rather than silently narrowed.
fn list(store: &Store, args: &Value, caller: Caller) -> CommandResult {
    match args.get("mine") {
        None | Some(Value::Null) | Some(Value::Bool(true)) => {}
        Some(Value::Bool(false)) => {
            return Err(CommandFailure::invalid(
                "`mine: false` is not available: workload.list lists only the caller's own workloads",
            ));
        }
        Some(other) => {
            return Err(CommandFailure::invalid(format!(
                "`mine` must be a boolean: {other:?}"
            )));
        }
    }
    let rows = store
        .workloads_for(caller)
        .map_err(CommandFailure::storage)?;
    let items: Vec<Value> = rows.iter().map(workload_summary).collect();
    Ok(serde_json::json!({ "workloads": items, "count": items.len() }))
}

fn show(store: &Store, args: &Value, caller: Caller) -> CommandResult {
    let id = parse_workload_id(args)?;
    let record = store
        .workload_for(caller, &id)
        .map_err(CommandFailure::storage)?
        .ok_or_else(|| CommandFailure {
            code: ErrorCode::NotFound,
            message: format!("workload not found: {id}"),
        })?;
    // The caller's own usage of this model, never other users' totals.
    let (tokens_in, tokens_out, cost_micros) = store
        .usage_totals_for(caller, &record.model_id)
        .map_err(CommandFailure::storage)?;
    let mut detail = workload_summary(&record);
    if let Some(fields) = detail.as_object_mut() {
        fields.insert("ceilings".into(), record.ceilings.clone());
        fields.insert(
            "usage_tokens_in_total_for_model".into(),
            serde_json::json!(tokens_in),
        );
        fields.insert(
            "usage_tokens_out_total_for_model".into(),
            serde_json::json!(tokens_out),
        );
        fields.insert(
            "usage_cost_micros_total_for_model".into(),
            serde_json::json!(cost_micros),
        );
    }
    Ok(detail)
}

fn parse_workload_id(args: &Value) -> Result<WorkloadId, CommandFailure> {
    let raw = args
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| CommandFailure::invalid("missing string arg `id`"))?;
    WorkloadId::from_str(raw)
        .map_err(|error| CommandFailure::invalid(format!("invalid workload id: {error}")))
}

fn transition(store: &mut Store, name: &str, args: &Value, caller: Caller) -> CommandResult {
    let id = parse_workload_id(args)?;
    let target = if name == "workload.stop" {
        WorkloadState::Stopped
    } else {
        WorkloadState::Completed
    };
    let actor = actor_from(args);
    let edges = vec![ProvenanceEdge {
        from: ObjectRef::from(&id),
        relation: ProvenanceRelation::CausedBy,
        to: ObjectRef::actor(&actor).map_err(|error| CommandFailure::invalid(error.to_string()))?,
        created_at: Timestamp::now(),
        note: Some(format!("workload {target}")),
    }];
    store
        .transition_workload_for(caller, &id, target, &edges)
        .map_err(CommandFailure::storage)?;
    Ok(serde_json::json!({ "id": id.as_str(), "state": target.to_string() }))
}

fn record_usage(store: &mut Store, args: &Value) -> CommandResult {
    let raw = args
        .get("workload_id")
        .and_then(Value::as_str)
        .ok_or_else(|| CommandFailure::invalid("missing string arg `workload_id`"))?;
    let workload_id = WorkloadId::from_str(raw)
        .map_err(|error| CommandFailure::invalid(format!("invalid workload id: {error}")))?;
    let record = store
        .get_workload(&workload_id)
        .map_err(CommandFailure::storage)?
        .ok_or_else(|| CommandFailure {
            code: ErrorCode::NotFound,
            message: format!("workload not found: {workload_id}"),
        })?;
    let get_u64 = |field: &str| -> u64 {
        args.get(field)
            .and_then(Value::as_u64)
            .expect("validated above")
    };
    let actor = actor_from(args);
    let usage = flowerly_core::types::UsageRecord {
        id: UsageId::new(),
        workload_id,
        model_id: record.model_id.clone(),
        tokens_in: get_u64("tokens_in"),
        tokens_out: get_u64("tokens_out"),
        latency_ms: get_u64("latency_ms"),
        cost_micros: get_u64("cost_micros"),
        created_by: actor.clone(),
        created_at: Timestamp::now(),
        // Usage belongs to the owner of the workload it accounts for.
        owner: record.owner,
    };
    store
        .insert_usage(&usage)
        .map_err(CommandFailure::storage)?;
    let (tokens_in, tokens_out, cost_micros) = store
        .usage_totals_per_model(&usage.model_id)
        .map_err(CommandFailure::storage)?;
    Ok(serde_json::json!({
        "id": usage.id.as_str(),
        "workload": usage.workload_id.as_str(),
        "model_totals": {
            "tokens_in": tokens_in,
            "tokens_out": tokens_out,
            "cost_micros": cost_micros,
        }
    }))
}

/// The caller's own usage of one model.
fn usage_brief(store: &Store, args: &Value, caller: Caller) -> CommandResult {
    let id = parse_model_id(args)?;
    let (tokens_in, tokens_out, cost_micros) = store
        .usage_totals_for(caller, &id)
        .map_err(CommandFailure::storage)?;
    Ok(serde_json::json!({
        "model": id.as_str(),
        "tokens_in": tokens_in,
        "tokens_out": tokens_out,
        "cost_micros": cost_micros,
        "promise": "ledger-only",
    }))
}

fn parse_model_id(args: &Value) -> Result<ModelId, CommandFailure> {
    let raw = args
        .get("model_id")
        .and_then(Value::as_str)
        .ok_or_else(|| CommandFailure::invalid("missing string arg `model_id`"))?;
    ModelId::from_str(raw)
        .map_err(|error| CommandFailure::invalid(format!("invalid model id: {error}")))
}

pub(crate) fn self_probe() -> Result<(), &'static str> {
    let pressure = MemoryPressure::measure();
    if let Some(avg10) = pressure.avg10_pct {
        if !(0.0..=100.0).contains(&avg10) {
            return Err("PSI avg10 outside the 0..100 range");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn psi_summary_parse_honest_none_on_missing() {
        let psi = "some other controller stuff\n\
                   full avg10=12.50 avg60=7.00 avg300=1.00 total=123456789\n";
        let pressure = MemoryPressure::from_text(psi).expect("parseable PSI");
        assert_eq!(pressure.avg10_pct, Some(12.5));
        // A truncated or unrelated composition must be None, never 0.
        assert!(
            MemoryPressure::from_text("other avg10=9999.99 avg60=0 avg300=0 total=9\n").is_none()
        );
        assert!(
            MemoryPressure::from_text("full avg10=not-a-number avg60=0 avg300=0 total=5\n")
                .is_none()
        );
    }

    #[test]
    fn headroom_guard_rejects_observed_shortfall_without_inventing_memory() {
        assert!(memory_headroom_reason(Some(2_000), Some(1_000))
            .unwrap()
            .contains("available host RAM"));
        assert_eq!(memory_headroom_reason(Some(1_000), Some(1_000)), None);
        assert_eq!(memory_headroom_reason(Some(1_000), Some(2_000)), None);
        assert_eq!(memory_headroom_reason(Some(1_000), None), None);
        assert_eq!(memory_headroom_reason(None, Some(1_000)), None);
    }

    #[test]
    fn accelerator_backends_are_refused_this_tranche() {
        assert!(accelerator_backend(ModelBackend::Cuda));
        assert!(accelerator_backend(ModelBackend::Vulkan));
        assert!(accelerator_backend(ModelBackend::Rocm));
        assert!(accelerator_backend(ModelBackend::Npu));
        assert!(!accelerator_backend(ModelBackend::Cpu));
        assert!(!accelerator_backend(ModelBackend::Unknown));
    }

    #[test]
    fn workload_state_round_trip() {
        for state in [
            WorkloadState::Queued,
            WorkloadState::Admitted,
            WorkloadState::Refused,
            WorkloadState::Stopped,
            WorkloadState::Completed,
        ] {
            let parsed: WorkloadState = state.to_string().parse().expect("parse");
            assert_eq!(parsed, state);
        }
        assert!("BROKEN".parse::<WorkloadState>().is_err());
    }
}
