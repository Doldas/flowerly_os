//! Expert AI profile commands and the inference provider registry boundary
//! (RFC 0004 tranche 2).
//!
//! Profile rules:
//! - TOML documents only; credential-shaped keys structurally refused.
//! - Append-only versioning; every mutation keeps an audit version/edge.
//! - Inheritance is explicit, acyclic and depth-bounded; locality can only
//!   tighten; ceilings never silently widen.
//!
//! Provider rules:
//! - Only `local-unix` providers can be registered today. Remote kinds are
//!   refused with `PermissionDenied` because no egress policy authorization
//!   exists; refusing fail-closed is the honest default.
//! - Endpoints must stay inside the daemon runtime directory (ADR-0010).

use flowerly_core::ipc::ErrorCode;
use flowerly_core::profile::{ProfileSpec, ProviderCapabilitySet};
use flowerly_core::storage::{MutationOperation, StorageError, Store};
use flowerly_core::types::Caller;
use flowerly_core::types::{
    ObjectRef, ProfileId, ProvenanceEdge, ProvenanceRelation, ProviderId, Timestamp,
};
use serde_json::Value;

use super::{actor_from, CommandResult};

const MAX_INHERITANCE_DEPTH: usize = 8;

fn spec_summary(spec: &ProfileSpec) -> Value {
    serde_json::json!({
        "capability_tier": spec.capability_tier.as_str(),
        "background_activity": spec.background_activity.as_str(),
        "locality": spec.locality,
        "fallback": spec.fallback,
        "allowed_models": spec.allowed_models,
        "preferred_backend": spec.preferred_backend,
        "memory_max_bytes": spec.memory_max_bytes,
        "vram_max_bytes": spec.vram_max_bytes,
        "concurrency_max": spec.concurrency_max,
    })
}

fn edge(
    id: &ProfileId,
    relation: ProvenanceRelation,
    actor: &str,
    note: &str,
) -> Option<ProvenanceEdge> {
    let edge = ProvenanceEdge {
        from: ObjectRef::from(id),
        relation,
        to: ObjectRef::actor(actor).ok()?,
        created_at: Timestamp::now(),
        note: Some(note.to_owned()),
    };
    Some(edge)
}

fn profile_failure(error: StorageError) -> super::CommandFailure {
    super::CommandFailure::storage(error)
}

fn idempotency_key_valid(raw: &str) -> bool {
    raw.len() <= 128
        && !raw.is_empty()
        && raw
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
}

fn validate_operation_id(args: &Value) -> Result<(), super::CommandFailure> {
    match args.get("operation_id") {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(raw)) if idempotency_key_valid(raw) => Ok(()),
        Some(other) => Err(super::CommandFailure::invalid(format!(
            "operation_id must follow the idempotency key rules: {other:?}"
        ))),
    }
}

fn validate_profile_name(name: &str) -> Result<(), super::CommandFailure> {
    if name.trim().is_empty() || name.len() > 128 {
        return Err(super::CommandFailure::invalid(
            "profile name must be 1..128 characters",
        ));
    }
    Ok(())
}

fn validate_toml(toml: &str) -> Result<ProfileSpec, super::CommandFailure> {
    if toml.len() > 16 * 1024 {
        return Err(super::CommandFailure::invalid(
            "profile TOML exceeds 16 KiB",
        ));
    }
    ProfileSpec::parse_toml(toml).map_err(super::CommandFailure::invalid)
}

pub(crate) fn run_profile(
    store: &mut Store,
    name: &str,
    args: &Value,
    caller: Caller,
) -> CommandResult {
    match name {
        "profile.create" | "profile.import" => {
            let actor = actor_from(args);
            let toml = args
                .get("toml")
                .and_then(Value::as_str)
                .ok_or_else(|| super::CommandFailure::invalid("missing string arg `toml`"))?;
            let spec = validate_toml(toml)?;
            validate_operation_id(args)?;
            let given_name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| super::CommandFailure::invalid("missing string arg `name`"))?;
            validate_profile_name(given_name)?;
            if let Some(base) = &spec.base {
                store
                    .profile_by_name(base)
                    .map_err(profile_failure)?
                    .ok_or_else(|| super::CommandFailure {
                        code: ErrorCode::NotFound,
                        message: format!("base profile not found: {base}"),
                    })?;
            }
            let id = ProfileId::new();
            let caused_note = if name == "profile.import" {
                "imported profile".to_owned()
            } else {
                "created profile".to_owned()
            };
            let edges = edge(&id, ProvenanceRelation::CreatedBy, &actor, &caused_note)
                .into_iter()
                .collect::<Vec<_>>();
            let summary_json = serde_json::to_string(&spec_summary(&spec))
                .map_err(|error| super::CommandFailure::invalid(error.to_string()))?;
            let intended_response = serde_json::json!({
                "id": id.as_str(),
                "name": given_name,
                "version": 1,
            });
            let response =
                if let Some(operation_id) = args.get("operation_id").and_then(Value::as_str) {
                    let request_json = serde_json::to_string(args)
                        .map_err(|error| super::CommandFailure::invalid(error.to_string()))?;
                    let response_json = serde_json::to_string(&intended_response)
                        .map_err(|error| super::CommandFailure::invalid(error.to_string()))?;
                    let operation = MutationOperation {
                        actor: &actor,
                        command: "profile.create",
                        operation_id,
                        request_payload: &request_json,
                        response_payload: &response_json,
                    };
                    let stored = store
                        .create_profile(
                            &id,
                            given_name,
                            &actor,
                            toml,
                            &summary_json,
                            caller.owner(),
                            Some(operation),
                            &edges,
                        )
                        .map_err(|error| match error {
                            ref other if other.is_unique_violation() => super::CommandFailure {
                                code: ErrorCode::Conflict,
                                message: "profile name already exists".to_owned(),
                            },
                            other => super::CommandFailure::storage(other),
                        })?;
                    if stored.is_empty() {
                        intended_response
                    } else {
                        serde_json::from_str(&stored)
                            .map_err(|error| super::CommandFailure::invalid(error.to_string()))?
                    }
                } else {
                    store
                        .create_profile(
                            &id,
                            given_name,
                            &actor,
                            toml,
                            &summary_json,
                            caller.owner(),
                            None,
                            &edges,
                        )
                        .map_err(|error| match error {
                            ref other if other.is_unique_violation() => super::CommandFailure {
                                code: ErrorCode::Conflict,
                                message: "profile name already exists".to_owned(),
                            },
                            other => super::CommandFailure::storage(other),
                        })?;
                    intended_response
                };
            tracing::info!(profile_id = %id, name = given_name, "profile recorded");
            Ok(response)
        }
        "profile.clone" => {
            let actor = actor_from(args);
            let source: ProfileId = args
                .get("id")
                .and_then(Value::as_str)
                .and_then(|raw| raw.parse().ok())
                .ok_or_else(|| {
                    super::CommandFailure::invalid("missing valid profile id arg `id`")
                })?;
            let new_name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| super::CommandFailure::invalid("missing string arg `name`"))?;
            validate_profile_name(new_name)?;
            validate_operation_id(args)?;
            let (_, toml, _) = store
                .profile_latest(&source)
                .map_err(profile_failure)?
                .ok_or_else(|| super::CommandFailure {
                    code: ErrorCode::NotFound,
                    message: format!("profile not found: {source}"),
                })?;
            let spec = validate_toml(&toml)?;
            let resolved = serde_json::to_string(&spec_summary(&spec))
                .map_err(|error| super::CommandFailure::invalid(error.to_string()))?;
            let id = ProfileId::new();
            let edges: Vec<ProvenanceEdge> = edge(
                &id,
                ProvenanceRelation::CausedBy,
                &actor,
                &format!("cloned from profile: {source}"),
            )
            .into_iter()
            .collect();
            let intended_response =
                serde_json::json!({ "id": id.as_str(), "name": new_name, "version": 1 });
            let clone_failure = |error: StorageError| match error {
                ref other if other.is_unique_violation() => super::CommandFailure {
                    code: ErrorCode::Conflict,
                    message: "profile name already exists".to_owned(),
                },
                other => super::CommandFailure::storage(other),
            };
            // An exact retry replays the recorded response (the first
            // clone's id); a reused key with other arguments is a conflict.
            if let Some(operation_id) = args.get("operation_id").and_then(Value::as_str) {
                let request_json = serde_json::to_string(args)
                    .map_err(|error| super::CommandFailure::invalid(error.to_string()))?;
                let response_json = serde_json::to_string(&intended_response)
                    .map_err(|error| super::CommandFailure::invalid(error.to_string()))?;
                let operation = MutationOperation {
                    actor: &actor,
                    command: "profile.clone",
                    operation_id,
                    request_payload: &request_json,
                    response_payload: &response_json,
                };
                let stored = store
                    .create_profile(
                        &id,
                        new_name,
                        &actor,
                        &toml,
                        &resolved,
                        caller.owner(),
                        Some(operation),
                        &edges,
                    )
                    .map_err(clone_failure)?;
                return serde_json::from_str(&stored)
                    .map_err(|error| super::CommandFailure::invalid(error.to_string()));
            }
            store
                .create_profile(
                    &id,
                    new_name,
                    &actor,
                    &toml,
                    &resolved,
                    caller.owner(),
                    None,
                    &edges,
                )
                .map_err(clone_failure)?;
            Ok(intended_response)
        }
        "profile.rename" => {
            let actor = actor_from(args);
            let id: ProfileId = args
                .get("id")
                .and_then(Value::as_str)
                .and_then(|raw| raw.parse().ok())
                .ok_or_else(|| {
                    super::CommandFailure::invalid("missing valid profile id arg `id`")
                })?;
            let new_name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| super::CommandFailure::invalid("missing string arg `name`"))?;
            validate_profile_name(new_name)?;
            // Renaming to a given name is idempotent by itself, so the key is
            // validated for a consistent contract but not recorded.
            validate_operation_id(args)?;
            store
                .rename_profile(&id, new_name, &actor)
                .map_err(profile_failure)?;
            Ok(serde_json::json!({ "id": id.as_str(), "name": new_name }))
        }
        "profile.edit" => {
            let actor = actor_from(args);
            let id: ProfileId = args
                .get("id")
                .and_then(Value::as_str)
                .and_then(|raw| raw.parse().ok())
                .ok_or_else(|| {
                    super::CommandFailure::invalid("missing valid profile id arg `id`")
                })?;
            let toml = args
                .get("toml")
                .and_then(Value::as_str)
                .ok_or_else(|| super::CommandFailure::invalid("missing string arg `toml`"))?;
            let spec = validate_toml(toml)?;
            validate_operation_id(args)?;
            let (name, _, _) = store
                .profile_row(&id)
                .map_err(profile_failure)?
                .ok_or_else(|| super::CommandFailure {
                    code: ErrorCode::NotFound,
                    message: format!("profile not found: {id}"),
                })?;
            if let Some(base) = &spec.base {
                if *base == name {
                    return Err(super::CommandFailure::invalid(
                        "a profile cannot be its own base",
                    ));
                }
                store
                    .profile_by_name(base)
                    .map_err(profile_failure)?
                    .ok_or_else(|| super::CommandFailure {
                        code: ErrorCode::NotFound,
                        message: format!("base profile not found: {base}"),
                    })?;
            }
            let summary = serde_json::to_string(&spec_summary(&spec))
                .map_err(|error| super::CommandFailure::invalid(error.to_string()))?;
            let edges: Vec<ProvenanceEdge> =
                edge(&id, ProvenanceRelation::CausedBy, &actor, "profile edit")
                    .into_iter()
                    .collect();
            let edit_failure = |error: StorageError| match error {
                ref other if other.is_unique_violation() => super::CommandFailure {
                    code: ErrorCode::Conflict,
                    message: "profile version already exists".to_owned(),
                },
                other => super::CommandFailure::storage(other),
            };
            // An exact retry replays the recorded version instead of
            // appending a duplicate; a reused key with other arguments is a
            // conflict.
            if let Some(operation_id) = args.get("operation_id").and_then(Value::as_str) {
                let request_json = serde_json::to_string(args)
                    .map_err(|error| super::CommandFailure::invalid(error.to_string()))?;
                let operation = MutationOperation {
                    actor: &actor,
                    command: "profile.edit",
                    operation_id,
                    request_payload: &request_json,
                    response_payload: "",
                };
                let stored = store
                    .append_profile_version_once(
                        &id,
                        toml,
                        &summary,
                        &actor,
                        &edges,
                        &operation,
                        |version| {
                            serde_json::json!({ "id": id.as_str(), "version": version }).to_string()
                        },
                    )
                    .map_err(edit_failure)?;
                return serde_json::from_str(&stored)
                    .map_err(|error| super::CommandFailure::invalid(error.to_string()));
            }
            let version = store
                .append_profile_version(&id, toml, &summary, &actor, &edges)
                .map_err(edit_failure)?;
            Ok(serde_json::json!({ "id": id.as_str(), "version": version }))
        }
        "profile.list" => {
            let rows = store.list_profiles().map_err(profile_failure)?;
            let items: Vec<Value> = rows
                .into_iter()
                .map(|(id, name, version, created_at)| {
                    serde_json::json!({
                        "id": id.as_str(),
                        "name": name,
                        "latest_version": version,
                        "created_at": created_at,
                    })
                })
                .collect();
            Ok(serde_json::json!({ "profiles": items, "count": items.len() }))
        }
        "profile.show" => {
            let (id, requested) = parse_profile_target(args)?;
            let (row_name, created_by, created_at) = store
                .profile_row(&id)
                .map_err(profile_failure)?
                .ok_or_else(|| super::CommandFailure {
                    code: ErrorCode::NotFound,
                    message: format!("profile not found: {id}"),
                })?;
            let (version, toml, summary) = resolve_version_toml_summary(store, &id, requested)?;
            Ok(serde_json::json!({
                "id": id.as_str(),
                "name": row_name,
                "created_by": created_by,
                "created_at": created_at,
                "version": version,
                "toml": toml,
                "summary": summary,
            }))
        }
        "profile.version_history" => {
            let id: ProfileId = args
                .get("id")
                .and_then(Value::as_str)
                .and_then(|raw| raw.parse().ok())
                .ok_or_else(|| {
                    super::CommandFailure::invalid("missing valid profile id arg `id`")
                })?;
            if store.profile_row(&id).map_err(profile_failure)?.is_none() {
                return Err(super::CommandFailure {
                    code: ErrorCode::NotFound,
                    message: format!("profile not found: {id}"),
                });
            }
            let mut versions: Vec<Value> = Vec::new();
            for version in 1..=512 {
                let Some((toml, summary, created_by, created_at)) = store
                    .profile_version(&id, version)
                    .map_err(profile_failure)?
                else {
                    break;
                };
                versions.push(serde_json::json!({
                    "version": version,
                    "created_by": created_by,
                    "created_at": created_at,
                    "summary": summary,
                    "toml": toml,
                }));
            }
            Ok(serde_json::json!({ "id": id.as_str(), "versions": versions }))
        }
        "profile.export" => {
            let (id, requested) = parse_profile_target(args)?;
            let (version, toml, _) = resolve_version_toml_summary(store, &id, requested)?;
            Ok(serde_json::json!({ "id": id.as_str(), "version": version, "toml": toml }))
        }
        "profile.resolve" => resolve_chain(store, args),
        other => Err(super::CommandFailure::invalid(format!(
            "compute: unknown command `{other}`"
        ))),
    }
}

fn parse_profile_target(args: &Value) -> Result<(ProfileId, Option<u32>), super::CommandFailure> {
    let id: ProfileId = args
        .get("id")
        .and_then(Value::as_str)
        .and_then(|raw| raw.parse().ok())
        .ok_or_else(|| super::CommandFailure::invalid("missing valid profile id arg `id`"))?;
    let requested = match args.get("version") {
        None | Some(Value::Null) => None,
        other => Some(
            other
                .and_then(Value::as_u64)
                .filter(|raw| *raw > 0 && *raw <= u64::from(u32::MAX))
                .ok_or_else(|| {
                    super::CommandFailure::invalid("`version` must be a positive integer")
                })? as u32,
        ),
    };
    Ok((id, requested))
}

/// Resolve a requested (or latest) profile version into
/// `(version, toml, summary)` with typed NotFound semantics.
fn resolve_version_toml_summary(
    store: &Store,
    id: &ProfileId,
    requested: Option<u32>,
) -> Result<(u32, String, String), super::CommandFailure> {
    match requested {
        Some(version) => {
            let (toml, summary, _, _) = store
                .profile_version(id, version)
                .map_err(profile_failure)?
                .ok_or_else(|| super::CommandFailure {
                    code: ErrorCode::NotFound,
                    message: format!("profile version not found: {id} v{version}"),
                })?;
            Ok((version, toml, summary))
        }
        None => store
            .profile_latest(id)
            .map_err(profile_failure)?
            .ok_or_else(|| super::CommandFailure {
                code: ErrorCode::NotFound,
                message: format!("profile has no versions: {id}"),
            }),
    }
}

/// Resolve the explicit inheritance chain to an effective spec. Parent links
/// resolve by **name** to each profile's latest version; cycles and depth
/// overflow are typed refusals. No credential or endpoint can appear in the
/// result because the document schema refuses them at parse time.
fn resolve_chain(store: &mut Store, args: &Value) -> CommandResult {
    let (start_id, requested) = parse_profile_target(args)?;
    let (name, _, _) = store
        .profile_row(&start_id)
        .map_err(profile_failure)?
        .ok_or_else(|| super::CommandFailure {
            code: ErrorCode::NotFound,
            message: format!("profile not found: {start_id}"),
        })?;
    if requested.is_some() {
        // Pinned-version resolution stops at exactly that version: history
        // must stay immutable, so parent links follow the stored document.
        let (_, spec_toml, _) = resolve_version_toml_summary(store, &start_id, requested)?;
        let spec = validate_toml(&spec_toml)?;
        spec.validate().map_err(super::CommandFailure::invalid)?;
        return Ok(serde_json::json!({
            "id": start_id.as_str(),
            "name": name,
            "chain": [name],
            "pinned_version": requested,
            "effective": spec_summary(&spec),
            "promise": "preference-data-only",
        }));
    }
    let (_, spec) = resolve_effective(store, &start_id)?;
    let mut chain = vec![name.clone()];
    if let Some(parent) = &spec.base {
        chain.push(parent.clone());
    }
    Ok(serde_json::json!({
        "id": start_id.as_str(),
        "name": name,
        "chain": chain,
        "effective": spec_summary(&spec),
        "promise": "preference-data-only",
    }))
}

/// Resolve a profile's effective spec across its explicit inheritance
/// chain (latest versions, depth-bounded, cycle-detecting). Returns the
/// display name and the effective merged spec.
pub(crate) fn resolve_effective(
    store: &Store,
    id: &ProfileId,
) -> Result<(String, ProfileSpec), super::CommandFailure> {
    let (name, _, _) = store
        .profile_row(id)
        .map_err(profile_failure)?
        .ok_or_else(|| super::CommandFailure {
            code: ErrorCode::NotFound,
            message: format!("profile not found: {id}"),
        })?;
    let (_, spec_toml, _) = store
        .profile_latest(id)
        .map_err(profile_failure)?
        .ok_or_else(|| super::CommandFailure {
            code: ErrorCode::NotFound,
            message: format!("profile has no versions: {id}"),
        })?;
    let mut spec = validate_toml(&spec_toml)?;
    let mut seen = vec![name.clone()];
    let mut base = spec.base.clone();
    let mut depth = 0;
    while let Some(parent_name) = base.take() {
        depth += 1;
        if depth > MAX_INHERITANCE_DEPTH {
            return Err(super::CommandFailure::invalid(format!(
                "profile inheritance exceeds depth {MAX_INHERITANCE_DEPTH}"
            )));
        }
        if seen.contains(&parent_name) {
            return Err(super::CommandFailure::invalid(format!(
                "profile inheritance cycle detected at `{parent_name}`"
            )));
        }
        seen.push(parent_name.clone());
        let (parent_id, _, _) = store
            .profile_by_name(&parent_name)
            .map_err(profile_failure)?
            .ok_or_else(|| super::CommandFailure {
                code: ErrorCode::NotFound,
                message: format!("base profile not found: {parent_name}"),
            })?;
        let (_, parent_toml, _) = store
            .profile_latest(&parent_id)
            .map_err(profile_failure)?
            .ok_or_else(|| super::CommandFailure {
                code: ErrorCode::NotFound,
                message: format!("base profile has no versions: {parent_name}"),
            })?;
        let parent = validate_toml(&parent_toml)?;
        base = parent.base.clone();
        spec = spec.resolve_over(&parent);
    }
    spec.validate().map_err(super::CommandFailure::invalid)?;
    Ok((name, spec))
}

const LOCAL_PROVIDER_KIND: &str = "local-unix";

pub(crate) fn run_provider(store: &mut Store, name: &str, args: &Value) -> CommandResult {
    match name {
        "provider.register" => {
            let actor = actor_from(args);
            let name_arg = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| super::CommandFailure::invalid("missing string arg `name`"))?;
            validate_profile_name(name_arg)?;
            // Absent or null defaults to local; any other non-string is a
            // caller error, never silently coerced to the local kind.
            let kind = match args.get("kind") {
                None | Some(Value::Null) => LOCAL_PROVIDER_KIND,
                Some(Value::String(kind)) => kind.as_str(),
                Some(other) => {
                    return Err(super::CommandFailure::invalid(format!(
                        "`kind` must be a string or null: {other:?}"
                    )));
                }
            };
            if kind != LOCAL_PROVIDER_KIND {
                return Err(super::CommandFailure {
                    code: ErrorCode::PermissionDenied,
                    message: format!(
                        "provider kind `{kind}` refused: no egress policy authorization exists; only `{LOCAL_PROVIDER_KIND}` may be registered"
                    ),
                });
            }
            let endpoint = match args.get("endpoint") {
                None | Some(Value::Null) => None,
                Some(Value::String(text)) => {
                    let trimmed = text.trim();
                    if trimmed.is_empty()
                        || trimmed != text
                        || trimmed.contains('/')
                        || trimmed.contains('\\')
                        || trimmed.contains("..")
                    {
                        return Err(super::CommandFailure::invalid(
                            "`endpoint` must be a bare filename inside the runtime directory",
                        ));
                    }
                    Some(text.clone())
                }
                Some(other) => {
                    return Err(super::CommandFailure::invalid(format!(
                        "`endpoint` must be a string or null: {other:?}"
                    )));
                }
            };
            let caps = args
                .get("capabilities")
                .and_then(Value::as_array)
                .and_then(|items| {
                    // A non-string entry rejects the request instead of
                    // being silently filtered out of the stored set.
                    items
                        .iter()
                        .map(|item| item.as_str().map(str::to_owned))
                        .collect::<Option<Vec<_>>>()
                })
                .ok_or_else(|| {
                    super::CommandFailure::invalid("missing string array arg `capabilities`")
                })?;
            if caps.is_empty() || caps.len() > 16 {
                return Err(super::CommandFailure::invalid(
                    "`capabilities` must hold 1..16 entries",
                ));
            }
            let capability_set = ProviderCapabilitySet { modalities: caps };
            validate_operation_id(args)?;
            let id = ProviderId::new();
            let edges: Vec<ProvenanceEdge> = vec![ProvenanceEdge {
                from: ObjectRef::from(&id),
                relation: ProvenanceRelation::CreatedBy,
                to: ObjectRef::actor(&actor)
                    .map_err(|error| super::CommandFailure::invalid(error.to_string()))?,
                created_at: Timestamp::now(),
                note: Some("registered local inference provider".to_owned()),
            }];
            let intended_response = serde_json::json!({ "id": id.as_str(), "kind": kind });
            // An exact retry replays the recorded response (the first
            // registration's id); a reused key with other arguments is a
            // conflict.
            let payloads = match args.get("operation_id").and_then(Value::as_str) {
                Some(operation_id) => Some((
                    operation_id,
                    serde_json::to_string(args)
                        .map_err(|error| super::CommandFailure::invalid(error.to_string()))?,
                    serde_json::to_string(&intended_response)
                        .map_err(|error| super::CommandFailure::invalid(error.to_string()))?,
                )),
                None => None,
            };
            let operation =
                payloads
                    .as_ref()
                    .map(|(operation_id, request, response)| MutationOperation {
                        actor: &actor,
                        command: "provider.register",
                        operation_id,
                        request_payload: request,
                        response_payload: response,
                    });
            let recorded = operation.is_some();
            let stored = store
                .register_provider(
                    &id,
                    name_arg,
                    kind,
                    endpoint.as_deref(),
                    &capability_set.modalities,
                    &actor,
                    operation,
                    &edges,
                )
                .map_err(|error| match error {
                    ref other if other.is_unique_violation() => super::CommandFailure {
                        code: ErrorCode::Conflict,
                        message: "provider name already exists".to_owned(),
                    },
                    other => super::CommandFailure::storage(other),
                })?;
            if recorded {
                return serde_json::from_str(&stored)
                    .map_err(|error| super::CommandFailure::invalid(error.to_string()));
            }
            Ok(intended_response)
        }
        "provider.list" => {
            let rows = store.list_providers().map_err(profile_failure)?;
            let items: Vec<Value> = rows
                .iter()
                .map(|provider| {
                    serde_json::json!({
                        "id": provider.id.as_str(),
                        "name": provider.name,
                        "kind": provider.kind,
                        "endpoint": provider.endpoint,
                        "capabilities": provider.capabilities,
                    })
                })
                .collect();
            Ok(serde_json::json!({ "providers": items, "count": items.len() }))
        }
        "provider.show" => {
            let id: ProviderId = args
                .get("id")
                .and_then(Value::as_str)
                .and_then(|raw| raw.parse().ok())
                .ok_or_else(|| {
                    super::CommandFailure::invalid("missing valid provider id arg `id`")
                })?;
            let provider = store
                .get_provider(&id)
                .map_err(profile_failure)?
                .ok_or_else(|| super::CommandFailure {
                    code: ErrorCode::NotFound,
                    message: format!("provider not found: {id}"),
                })?;
            Ok(serde_json::json!({
                "id": provider.id.as_str(),
                "name": provider.name,
                "kind": provider.kind,
                "endpoint": provider.endpoint,
                "capabilities": provider.capabilities,
                "created_by": provider.created_by,
                "created_at": provider.created_at.as_millis(),
            }))
        }
        other => Err(super::CommandFailure::invalid(format!(
            "compute: unknown command `{other}`"
        ))),
    }
}
