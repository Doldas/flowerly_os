use std::str::FromStr;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use flowerly_core::ipc::{Envelope, ErrorCode, Payload};
use flowerly_core::storage::repository::{CoreCommandRepository, CoreQueryRepository, CreateFlow};
use flowerly_core::storage::{FlowView, MutationOperation, StorageError, Store};
use flowerly_core::types::Caller;
use flowerly_core::types::{
    Flow, FlowId, FlowState, ObjectRef, ProvenanceEdge, ProvenanceRelation, Timestamp,
};
use flowerly_core::{coded_error_envelope, error_envelope, ok_envelope};
use serde::Deserialize;
use serde_json::Value;

fn ensure_general_flow(store: &mut Store) -> anyhow::Result<()> {
    let id = store.ensure_general_flow()?;
    tracing::debug!(flow_id = %id, "General system Flow verified");
    Ok(())
}

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
        Err(_) => {
            return error_envelope(request.id.clone(), "storage lock poisoned");
        }
    };
    if let Err((code, message)) = validate_flow_command(&store, name, args, caller) {
        return coded_error_envelope(request.id.clone(), code, message);
    }
    let data = match run_command(&mut store, name, args, caller) {
        Ok(data) => data,
        Err(error) => {
            return coded_error_envelope(request.id.clone(), error.code, error.message);
        }
    };
    ok_envelope(request.id.clone(), data)
}

/// Typed IPC-v1 arguments (see `crate::args` for the shared contract).
#[derive(Debug, Deserialize)]
struct CreateFlowArgs {
    name: String,
    #[serde(default)]
    operation_id: Option<String>,
}

/// Lookup by `id` (preferred) or `name`; `null` means absent.
#[derive(Debug, Deserialize)]
struct FlowRefArgs {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FlowListArgs {}

/// Classify user-facing Flow contract failures before mutation, while the
/// storage lock is held. Database errors are never inferred from text.
fn validate_flow_command(
    store: &Store,
    command: &str,
    args: &Value,
    caller: Caller,
) -> Result<(), (ErrorCode, String)> {
    let invalid = |message: &str| (ErrorCode::InvalidArgument, message.to_owned());
    match command {
        "flow.create" => {
            let typed: CreateFlowArgs = crate::args::typed_args("flow", args)?;
            // Type-checked here; replay semantics are applied by the handler.
            let _ = &typed.operation_id;
            let name = typed.name.as_str();
            if name.trim().is_empty() {
                return Err(invalid("flow name must not be empty"));
            }
            if name == "General" {
                return Err((
                    ErrorCode::Conflict,
                    "General is reserved for the system Flow".to_owned(),
                ));
            }
        }
        "flow.list" => {
            let _: FlowListArgs = crate::args::typed_args("flow", args)?;
        }
        "flow.activate" | "flow.archive" => {
            let typed: FlowRefArgs = crate::args::typed_args("flow", args)?;
            if typed.id.is_none() && typed.name.is_none() {
                return Err(invalid("provide either `id` or `name`"));
            }
            if let Some(raw) = typed.id.as_deref() {
                FlowId::from_str(raw).map_err(|e| invalid(&e.to_string()))?;
            }
            // Visibility, General and archived checks use the caller's view
            // and are repeated inside the mutating transaction.
            let view = find_flow(store, args, caller).map_err(|e| (e.code, e.message))?;
            if command == "flow.archive" && view.is_general() {
                return Err((
                    ErrorCode::PermissionDenied,
                    "General cannot be archived".to_owned(),
                ));
            }
            if command == "flow.activate" && view.flow.state == FlowState::Archived {
                return Err((
                    ErrorCode::Conflict,
                    format!("archived flow: {}", view.flow.id),
                ));
            }
        }
        other => {
            return Err((
                ErrorCode::InvalidArgument,
                format!("flowd: unknown command `{other}`"),
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

#[cfg(test)]
impl CommandFailure {
    fn contains(&self, pattern: &str) -> bool {
        self.message.contains(pattern)
    }
}

impl CommandFailure {
    fn storage(error: StorageError) -> Self {
        Self {
            code: storage_error_code(&error),
            message: error.to_string(),
        }
    }
}

impl std::fmt::Display for CommandFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl PartialEq<&str> for CommandFailure {
    fn eq(&self, other: &&str) -> bool {
        self.message == *other
    }
}

impl PartialEq<String> for CommandFailure {
    fn eq(&self, other: &String) -> bool {
        self.message == *other
    }
}

fn run_command(
    store: &mut Store,
    name: &str,
    args: &Value,
    caller: Caller,
) -> Result<Value, CommandFailure> {
    match name {
        "flow.create" => {
            if args
                .get("operation_id")
                .is_some_and(|value| !value.is_null() && !value.is_string())
            {
                return Err(CommandFailure {
                    code: ErrorCode::InvalidArgument,
                    message: "operation_id must be a string or null".to_owned(),
                });
            }
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or("missing string arg `name`")?;
            let created_by = args
                .get("created_by")
                .and_then(Value::as_str)
                .unwrap_or("user");
            if name.trim().is_empty() {
                return Err("flow name must not be empty".into());
            }
            if name == "General" {
                return Err("General is reserved for the system Flow".into());
            }
            let mut flow = Flow::new(name, created_by);
            flow.owner = caller.owner();
            let provenance = ProvenanceEdge {
                from: ObjectRef::from(&flow.id),
                relation: ProvenanceRelation::CreatedBy,
                to: ObjectRef::actor(created_by).map_err(err_str)?,
                created_at: Timestamp::now(),
                note: None,
            };
            let intended_response =
                serde_json::json!({ "id": flow.id, "name": flow.name, "state": flow.state });
            let response = if let Some(operation_id) =
                args.get("operation_id").and_then(Value::as_str)
            {
                let request_json = serde_json::to_string(args).map_err(err_str)?;
                let response_json = serde_json::to_string(&intended_response).map_err(err_str)?;
                let operation = MutationOperation {
                    actor: created_by,
                    command: "flow.create",
                    operation_id,
                    request_payload: &request_json,
                    response_payload: &response_json,
                };
                let stored = CoreCommandRepository::create_flow_once(
                    store,
                    CreateFlow {
                        flow: &flow,
                        provenance: std::slice::from_ref(&provenance),
                    },
                    &operation,
                )
                .map_err(CommandFailure::storage)?;
                serde_json::from_str(&stored).map_err(err_str)?
            } else {
                CoreCommandRepository::create_flow(
                    store,
                    CreateFlow {
                        flow: &flow,
                        provenance: std::slice::from_ref(&provenance),
                    },
                )
                .map_err(CommandFailure::storage)?;
                intended_response
            };
            tracing::info!(flow_id = %response["id"], flow_name = name, "flow create completed");
            Ok(response)
        }
        "flow.list" => {
            let flows =
                CoreQueryRepository::flows(store, caller).map_err(CommandFailure::storage)?;
            let items: Vec<Value> = flows.iter().map(flow_json).collect();
            Ok(serde_json::json!({ "flows": items, "count": items.len() }))
        }
        "flow.activate" | "flow.archive" => {
            let target = find_flow(store, args, caller)?;
            let view = if name == "flow.activate" {
                CoreCommandRepository::activate_flow(store, caller, &target.flow.id)
            } else {
                CoreCommandRepository::archive_flow(store, caller, &target.flow.id)
            }
            .map_err(CommandFailure::storage)?;
            Ok(flow_json(&view))
        }
        other => Err(format!("flowd: unknown command `{other}`").into()),
    }
}

/// A Flow as the caller sees it: lifecycle `state` and, separately, whether
/// it is the caller's own effective `foreground` (#225).
fn flow_json(view: &FlowView) -> Value {
    serde_json::json!({
        "id": view.flow.id,
        "name": view.flow.name,
        "state": view.flow.state,
        "foreground": view.foreground,
    })
}

/// Resolve `id` (preferred) or `name` among the Flows visible to `caller`.
/// Another owner's Flow is reported exactly like a missing one.
fn find_flow(store: &Store, args: &Value, caller: Caller) -> Result<FlowView, CommandFailure> {
    let not_found = |what: &str| CommandFailure {
        code: ErrorCode::NotFound,
        message: format!("flow not found: {what}"),
    };
    if let Some(value) = args.get("id").filter(|value| !value.is_null()) {
        let raw = value.as_str().ok_or(CommandFailure {
            code: ErrorCode::InvalidArgument,
            message: "arg `id` must be a string".to_owned(),
        })?;
        let id = FlowId::from_str(raw).map_err(|error| CommandFailure {
            code: ErrorCode::InvalidArgument,
            message: error.to_string(),
        })?;
        return CoreQueryRepository::flow(store, caller, &id)
            .map_err(CommandFailure::storage)?
            .ok_or_else(|| not_found(id.as_str()));
    }
    if let Some(by_name) = args.get("name").and_then(Value::as_str) {
        let flows = CoreQueryRepository::flows(store, caller).map_err(CommandFailure::storage)?;
        let mut matches = flows.into_iter().filter(|view| view.flow.name == by_name);
        let found = matches.next().ok_or_else(|| not_found(by_name))?;
        if matches.next().is_some() {
            return Err(CommandFailure {
                code: ErrorCode::Conflict,
                message: format!("ambiguous flow name: {by_name}; use id"),
            });
        }
        return Ok(found);
    }
    Err(CommandFailure {
        code: ErrorCode::InvalidArgument,
        message: "provide either `id` or `name`".to_owned(),
    })
}

fn err_str(error: impl std::fmt::Display) -> String {
    error.to_string()
}

pub(crate) async fn self_test() -> anyhow::Result<()> {
    let dir = tempfile::tempdir().context("creating temp dir")?;
    let db = dir.path().join("flowd-self-test.db");
    let mut store = Store::open(&db).context("opening self-test db")?;
    let applied = store.migrate()?;
    ensure_general_flow(&mut store)?;
    let flows = store.list_flows()?;
    let flow = Flow::new("Development", "self-test");
    store.insert_flow(&flow)?;
    store.set_flow_state(&flow.id, FlowState::Active)?;
    let reloaded = store.get_flow(&flow.id)?.expect("flow persisted");
    assert_eq!(reloaded.state, FlowState::Active);
    println!(
        "flowd self-test ok: migrations={} flows={}",
        applied.len(),
        flows.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CALLER: Caller = Caller::new(1000);

    fn migrated_store() -> Store {
        let mut store = Store::open_in_memory().expect("open store");
        store.migrate().expect("migrate store");
        store
    }

    fn owned(name: &str, caller: Caller) -> Flow {
        let mut flow = Flow::new(name, caller.actor());
        flow.owner = caller.owner();
        flow
    }

    #[test]
    fn malformed_flow_id_is_rejected_instead_of_falling_back() {
        let store = migrated_store();
        let error = find_flow(
            &store,
            &serde_json::json!({ "id": "not-a-flow-id" }),
            TEST_CALLER,
        )
        .expect_err("invalid flow id must fail");

        assert!(
            error.contains("flow"),
            "unexpected invalid-id error: {error}"
        );
    }

    #[test]
    fn activation_is_per_owner_and_records_activity() {
        let mut store = migrated_store();
        ensure_general_flow(&mut store).unwrap();
        let other = Caller::new(2000);
        let a = owned("Alpha", TEST_CALLER);
        let b = owned("Beta", TEST_CALLER);
        store.insert_flow(&a).unwrap();
        store.insert_flow(&b).unwrap();
        let activate = |store: &mut Store, id: &FlowId| {
            run_command(
                store,
                "flow.activate",
                &serde_json::json!({ "id": id }),
                TEST_CALLER,
            )
            .unwrap()
        };
        activate(&mut store, &a.id);
        let reply = activate(&mut store, &b.id);
        assert_eq!(reply["foreground"], true);
        assert_eq!(reply["state"], "ACTIVE", "lifecycle state is separate");
        let current = store.get_flow(&b.id).unwrap().unwrap();
        assert!(current.last_active.is_some());
        assert!(current.revision > b.revision);
        let list =
            run_command(&mut store, "flow.list", &serde_json::json!({}), TEST_CALLER).unwrap();
        let foreground: Vec<_> = list["flows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|flow| flow["foreground"] == true)
            .map(|flow| flow["name"].clone())
            .collect();
        assert_eq!(foreground, ["Beta"]);
        // Another uid still has General and cannot see either Flow.
        let theirs = run_command(&mut store, "flow.list", &serde_json::json!({}), other).unwrap();
        assert_eq!(
            theirs["flows"],
            serde_json::json!([{"id": "flow-general", "name": "General",
                                "state": "ACTIVE", "foreground": true}])
        );
    }

    #[test]
    fn another_owners_flow_is_not_found_by_id_or_name() {
        let mut store = migrated_store();
        ensure_general_flow(&mut store).unwrap();
        let theirs = owned("Private", Caller::new(2000));
        store.insert_flow(&theirs).unwrap();
        for args in [
            serde_json::json!({ "id": theirs.id }),
            serde_json::json!({ "name": "Private" }),
        ] {
            for command in ["flow.activate", "flow.archive"] {
                let (code, message) =
                    validate_flow_command(&store, command, &args, TEST_CALLER).unwrap_err();
                assert_eq!(code, ErrorCode::NotFound, "{command} {args}");
                assert!(message.starts_with("flow not found: "), "{message}");
                let failure = run_command(&mut store, command, &args, TEST_CALLER).unwrap_err();
                assert_eq!(failure.code, ErrorCode::NotFound);
            }
        }
        assert_eq!(
            store.get_flow(&theirs.id).unwrap().unwrap().state,
            FlowState::Active
        );
    }

    #[test]
    fn ambiguous_names_and_general_archive_are_rejected() {
        let mut store = migrated_store();
        ensure_general_flow(&mut store).unwrap();
        for _ in 0..2 {
            store.insert_flow(&owned("Duplicate", TEST_CALLER)).unwrap();
        }
        // Another owner's same-named Flow is invisible, not ambiguous.
        store
            .insert_flow(&owned("Mine", Caller::new(2000)))
            .unwrap();
        store.insert_flow(&owned("Mine", TEST_CALLER)).unwrap();
        assert!(find_flow(&store, &serde_json::json!({"name":"Mine"}), TEST_CALLER).is_ok());
        assert!(find_flow(
            &store,
            &serde_json::json!({"name":"Duplicate"}),
            TEST_CALLER
        )
        .unwrap_err()
        .contains("ambiguous"));
        assert!(run_command(
            &mut store,
            "flow.archive",
            &serde_json::json!({"name":"General"}),
            TEST_CALLER
        )
        .unwrap_err()
        .message
        .contains("cannot be archived"));
    }

    #[test]
    fn flow_errors_have_explicit_codes() {
        let mut store = migrated_store();
        ensure_general_flow(&mut store).unwrap();
        let cases = [
            (
                "flow.create",
                serde_json::json!({"name": "General"}),
                ErrorCode::Conflict,
            ),
            (
                "flow.activate",
                serde_json::json!({"id": "flow-missing"}),
                ErrorCode::NotFound,
            ),
            (
                "flow.archive",
                serde_json::json!({"name": "General"}),
                ErrorCode::PermissionDenied,
            ),
            (
                "flow.activate",
                serde_json::json!({"id": 42}),
                ErrorCode::InvalidArgument,
            ),
        ];
        for (command, args, expected) in cases {
            assert_eq!(
                validate_flow_command(&store, command, &args, TEST_CALLER)
                    .unwrap_err()
                    .0,
                expected
            );
        }
        for _ in 0..2 {
            store.insert_flow(&owned("Collision", TEST_CALLER)).unwrap();
        }
        let ambiguous = validate_flow_command(
            &store,
            "flow.activate",
            &serde_json::json!({"name": "Collision"}),
            TEST_CALLER,
        );
        assert_eq!(ambiguous.unwrap_err().0, ErrorCode::Conflict);
    }

    #[test]
    fn non_string_flow_id_is_rejected() {
        let store = migrated_store();
        let error = find_flow(&store, &serde_json::json!({ "id": 42 }), TEST_CALLER)
            .expect_err("non-string flow id must fail");

        assert_eq!(error, "arg `id` must be a string");
    }

    fn ipc(store: &Arc<Mutex<Store>>, command: &str, args: Value) -> Result<Value, ErrorCode> {
        let request = Envelope::new(Payload::Command {
            name: command.into(),
            args,
        });
        match handle_command(store, &request, TEST_CALLER).payload {
            Payload::Ok { data } => Ok(data),
            Payload::Err { code, .. } => Err(code.expect("typed error code")),
            other => panic!("unexpected reply {other:?}"),
        }
    }

    #[test]
    fn typed_v1_arguments_contract() {
        let mut db = migrated_store();
        ensure_general_flow(&mut db).unwrap();
        let store = Arc::new(Mutex::new(db));
        // Unknown fields are ignored: older and newer clients keep working.
        let created = ipc(
            &store,
            "flow.create",
            serde_json::json!({"name": "Typed", "client_hint": {"v": 2}, "operation_id": null}),
        )
        .expect("create with unknown field");
        // Explicit null means absent: `id: null` falls back to `name`.
        ipc(
            &store,
            "flow.activate",
            serde_json::json!({"id": null, "name": "Typed"}),
        )
        .expect("null id is absent");
        ipc(&store, "flow.list", serde_json::json!({"extra": true})).expect("list");
        // Malformed known fields and non-object arguments are invalid_argument.
        for (command, args) in [
            ("flow.create", serde_json::json!({"name": 7})),
            (
                "flow.create",
                serde_json::json!({"name": "X", "operation_id": 5}),
            ),
            ("flow.activate", serde_json::json!({"name": 7})),
            ("flow.activate", serde_json::json!({"id": ["flow"]})),
            (
                "flow.archive",
                serde_json::json!({"id": "goal-00000000000000000000000000000000"}),
            ),
            ("flow.list", serde_json::json!([])),
            ("flow.create", serde_json::json!("Typed")),
        ] {
            assert_eq!(
                ipc(&store, command, args.clone()),
                Err(ErrorCode::InvalidArgument),
                "{command} {args}"
            );
        }
        assert_eq!(created["name"], "Typed");
    }
}
