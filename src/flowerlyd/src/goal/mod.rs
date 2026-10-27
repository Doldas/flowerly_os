use std::str::FromStr;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use flowerly_core::ipc::{Envelope, ErrorCode, Payload};
use flowerly_core::storage::repository::{CoreCommandRepository, CoreQueryRepository, CreateGoal};
use flowerly_core::storage::{MutationOperation, StorageError, Store};
use flowerly_core::types::Caller;
use flowerly_core::types::{
    Goal, GoalId, GoalStatus, ObjectRef, ProvenanceEdge, ProvenanceRelation, Timestamp,
};
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
    if let Err((code, message)) = validate_goal_command(&store, name, args, caller) {
        return coded_error_envelope(request.id.clone(), code, message);
    }
    match run_command(&mut store, name, args, caller) {
        Ok(data) => ok_envelope(request.id.clone(), data),
        Err(error) => coded_error_envelope(request.id.clone(), error.code, error.message),
    }
}

/// Typed IPC-v1 arguments (see `crate::args` for the shared contract).
#[derive(Debug, Deserialize)]
struct CreateGoalArgs {
    objective: String,
    #[serde(default)]
    priority: Option<i64>,
    #[serde(default)]
    rationale: Option<String>,
    #[serde(default)]
    operation_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChildGoalArgs {
    id: String,
    objective: String,
    #[serde(default)]
    operation_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoalIdArgs {
    id: String,
}

#[derive(Debug, Deserialize)]
struct GoalListArgs {}

/// Validate client-facing Goal errors while the store lock is held.
/// SQLite errors remain internal instead of being inferred from error messages.
fn validate_goal_command(
    store: &Store,
    name: &str,
    args: &Value,
    caller: Caller,
) -> Result<(), (ErrorCode, String)> {
    let invalid = |message: &str| (ErrorCode::InvalidArgument, message.to_owned());
    let raw = match name {
        "goal.create" => {
            let typed: CreateGoalArgs = crate::args::typed_args("goal", args)?;
            // Type-checked here; the handler applies them.
            let _ = (&typed.objective, &typed.rationale, &typed.operation_id);
            if let Some(priority) = typed.priority {
                i32::try_from(priority).map_err(|_| invalid("priority out of range for i32"))?;
            }
            return Ok(());
        }
        "goal.list" => {
            let _: GoalListArgs = crate::args::typed_args("goal", args)?;
            return Ok(());
        }
        "goal.child" => {
            let typed: ChildGoalArgs = crate::args::typed_args("goal", args)?;
            let _ = (&typed.objective, &typed.operation_id);
            typed.id
        }
        "goal.show" | "goal.complete" | "goal.abandon" => {
            let typed: GoalIdArgs = crate::args::typed_args("goal", args)?;
            typed.id
        }
        other => {
            return Err((
                ErrorCode::InvalidArgument,
                format!("goald: unknown command `{other}`"),
            ));
        }
    };
    let id = GoalId::from_str(&raw).map_err(|error| invalid(&error.to_string()))?;
    // Another owner's Goal is reported exactly like a missing one (#225).
    if CoreQueryRepository::goal(store, caller, &id)
        .map_err(|error| (storage_error_code(&error), error.to_string()))?
        .is_none()
    {
        return Err((ErrorCode::NotFound, format!("goal not found: {raw}")));
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

#[cfg(test)]
impl CommandFailure {
    fn contains(&self, pattern: &str) -> bool {
        self.message.contains(pattern)
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
        "goal.create" => {
            if args
                .get("operation_id")
                .is_some_and(|value| !value.is_null() && !value.is_string())
            {
                return Err(CommandFailure {
                    code: ErrorCode::InvalidArgument,
                    message: "operation_id must be a string or null".to_owned(),
                });
            }
            let objective = args
                .get("objective")
                .and_then(Value::as_str)
                .ok_or("missing string arg `objective`")?;
            let created_by = args
                .get("created_by")
                .and_then(Value::as_str)
                .unwrap_or("user");
            let mut goal = Goal::new(objective, created_by);
            goal.owner = caller.owner();
            if let Some(value) = args.get("priority").filter(|value| !value.is_null()) {
                let priority = value.as_i64().ok_or("arg `priority` must be an integer")?;
                goal.priority = i32::try_from(priority)
                    .map_err(|_| format!("priority out of range for i32: {priority}"))?;
            }
            goal.rationale = args
                .get("rationale")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let created_edge = ProvenanceEdge {
                from: ObjectRef::from(&goal.id),
                relation: ProvenanceRelation::CreatedBy,
                to: ObjectRef::actor(created_by).map_err(err_str)?,
                created_at: Timestamp::now(),
                note: None,
            };
            let intended_response = serde_json::json!({
                "id": goal.id,
                "objective": goal.objective,
                "status": goal.status,
                "parent_goal": goal.parent_goal,
            });
            let response = if let Some(operation_id) =
                args.get("operation_id").and_then(Value::as_str)
            {
                let request_json = serde_json::to_string(args).map_err(err_str)?;
                let response_json = serde_json::to_string(&intended_response).map_err(err_str)?;
                let operation = MutationOperation {
                    actor: created_by,
                    command: "goal.create",
                    operation_id,
                    request_payload: &request_json,
                    response_payload: &response_json,
                };
                let stored = CoreCommandRepository::create_goal_once(
                    store,
                    CreateGoal {
                        goal: &goal,
                        provenance: std::slice::from_ref(&created_edge),
                    },
                    &operation,
                )
                .map_err(CommandFailure::storage)?;
                serde_json::from_str(&stored).map_err(err_str)?
            } else {
                CoreCommandRepository::create_goal(
                    store,
                    CreateGoal {
                        goal: &goal,
                        provenance: std::slice::from_ref(&created_edge),
                    },
                )
                .map_err(CommandFailure::storage)?;
                intended_response
            };
            tracing::info!(goal_id = %response["id"], objective, "goal create completed");
            Ok(response)
        }
        "goal.child" => {
            if args
                .get("operation_id")
                .is_some_and(|value| !value.is_null() && !value.is_string())
            {
                return Err(CommandFailure {
                    code: ErrorCode::InvalidArgument,
                    message: "operation_id must be a string or null".to_owned(),
                });
            }
            let parent_id = args
                .get("id")
                .and_then(Value::as_str)
                .ok_or("missing string arg `id`")?;
            let parent = GoalId::from_str(parent_id).map_err(err_str)?;
            // Rechecked in the inserting transaction (same owner only).
            if CoreQueryRepository::goal(store, caller, &parent)
                .map_err(CommandFailure::storage)?
                .is_none()
            {
                return Err(CommandFailure {
                    code: ErrorCode::NotFound,
                    message: format!("goal not found: {parent}"),
                });
            }
            let objective = args
                .get("objective")
                .and_then(Value::as_str)
                .ok_or("missing string arg `objective`")?;
            let created_by = args
                .get("created_by")
                .and_then(Value::as_str)
                .unwrap_or("agent");
            let mut child = Goal::new(objective, created_by);
            child.owner = caller.owner();
            child.parent_goal = Some(parent.clone());
            let created_edge = ProvenanceEdge {
                from: ObjectRef::from(&child.id),
                relation: ProvenanceRelation::CreatedBy,
                to: ObjectRef::actor(created_by).map_err(err_str)?,
                created_at: Timestamp::now(),
                note: None,
            };
            let parent_edge = ProvenanceEdge {
                from: ObjectRef::from(&child.id),
                relation: ProvenanceRelation::BelongsTo,
                to: ObjectRef::from(&parent),
                created_at: Timestamp::now(),
                note: Some("child goal".to_owned()),
            };
            let intended_response = serde_json::json!({ "id": child.id, "objective": child.objective, "parent": parent });
            let response = if let Some(operation_id) =
                args.get("operation_id").and_then(Value::as_str)
            {
                let request_json = serde_json::to_string(args).map_err(err_str)?;
                let response_json = serde_json::to_string(&intended_response).map_err(err_str)?;
                let operation = MutationOperation {
                    actor: created_by,
                    command: "goal.child",
                    operation_id,
                    request_payload: &request_json,
                    response_payload: &response_json,
                };
                let stored = CoreCommandRepository::create_goal_once(
                    store,
                    CreateGoal {
                        goal: &child,
                        provenance: &[created_edge, parent_edge],
                    },
                    &operation,
                )
                .map_err(CommandFailure::storage)?;
                serde_json::from_str(&stored).map_err(err_str)?
            } else {
                CoreCommandRepository::create_goal(
                    store,
                    CreateGoal {
                        goal: &child,
                        provenance: &[created_edge, parent_edge],
                    },
                )
                .map_err(CommandFailure::storage)?;
                intended_response
            };
            Ok(response)
        }
        "goal.show" => {
            let id = args
                .get("id")
                .and_then(Value::as_str)
                .ok_or("missing string arg `id`")?;
            let goal =
                CoreQueryRepository::goal(store, caller, &GoalId::from_str(id).map_err(err_str)?)
                    .map_err(CommandFailure::storage)?
                    .ok_or_else(|| CommandFailure {
                        code: ErrorCode::NotFound,
                        message: format!("goal not found: {id}"),
                    })?;
            Ok(serde_json::json!({
                "id": goal.id,
                "parent_goal": goal.parent_goal,
                "objective": goal.objective,
                "status": goal.status,
                "priority": goal.priority,
                "created_by": goal.created_by,
            }))
        }
        "goal.list" => {
            let goals =
                CoreQueryRepository::goals(store, caller).map_err(CommandFailure::storage)?;
            let items: Vec<Value> = goals
                .iter()
                .map(|g| {
                    serde_json::json!({
                        "id": g.id,
                        "objective": g.objective,
                        "status": g.status,
                        "parent_goal": g.parent_goal,
                    })
                })
                .collect();
            Ok(serde_json::json!({ "goals": items, "count": items.len() }))
        }
        "goal.complete" | "goal.abandon" => {
            let id = args
                .get("id")
                .and_then(Value::as_str)
                .ok_or("missing string arg `id`")?;
            let goal_id = GoalId::from_str(id).map_err(err_str)?;
            let status = if name == "goal.complete" {
                GoalStatus::Completed
            } else {
                GoalStatus::Abandoned
            };
            CoreCommandRepository::set_goal_status(store, caller, &goal_id, status)
                .map_err(CommandFailure::storage)?;
            Ok(serde_json::json!({ "id": goal_id, "status": status }))
        }
        other => Err(format!("goald: unknown command `{other}`").into()),
    }
}

fn err_str(error: impl std::fmt::Display) -> String {
    error.to_string()
}

pub(crate) async fn self_test() -> anyhow::Result<()> {
    let dir = tempfile::tempdir().context("creating temp dir")?;
    let db = dir.path().join("goald-self-test.db");
    let mut store = Store::open(&db).context("opening self-test db")?;
    let applied = store.migrate()?;
    let parent = Goal::new("Improve developer environment", "self-test");
    store.insert_goal(&parent)?;
    let mut child = Goal::new("Reduce build latency", "agent:planner");
    child.parent_goal = Some(parent.id.clone());
    store.insert_goal(&child)?;
    store.set_goal_status(&parent.id, GoalStatus::Active)?;
    assert_eq!(store.list_goals()?.len(), 2);
    println!("goald self-test ok: migrations={}", applied.len());
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

    #[test]
    fn goal_contract_failures_use_typed_error_codes() {
        let store = migrated_store();
        let cases = [
            (
                "goal.create",
                serde_json::json!({}),
                ErrorCode::InvalidArgument,
            ),
            (
                "goal.create",
                serde_json::json!({"objective": "hello", "priority": "high"}),
                ErrorCode::InvalidArgument,
            ),
            (
                "goal.create",
                serde_json::json!({"objective": "hello", "priority": i64::MAX}),
                ErrorCode::InvalidArgument,
            ),
            (
                "goal.child",
                serde_json::json!({"id": "goal-missing", "objective": "child"}),
                ErrorCode::NotFound,
            ),
            (
                "goal.show",
                serde_json::json!({"id": "goal-missing"}),
                ErrorCode::NotFound,
            ),
            (
                "goal.complete",
                serde_json::json!({"id": 42}),
                ErrorCode::InvalidArgument,
            ),
            ("unknown", serde_json::json!({}), ErrorCode::InvalidArgument),
        ];
        for (command, args, expected) in cases {
            assert_eq!(
                validate_goal_command(&store, command, &args, TEST_CALLER)
                    .unwrap_err()
                    .0,
                expected
            );
        }
        assert!(validate_goal_command(
            &store,
            "goal.create",
            &serde_json::json!({"objective": "valid", "priority": null}),
            TEST_CALLER
        )
        .is_ok());
    }

    #[test]
    fn null_priority_uses_model_default() {
        let mut store = migrated_store();
        let response = run_command(
            &mut store,
            "goal.create",
            &serde_json::json!({ "objective": "default priority", "priority": null }),
            TEST_CALLER,
        )
        .expect("null optional priority should use default");

        let id: GoalId = response["id"]
            .as_str()
            .expect("goal id")
            .parse()
            .expect("typed goal id");
        let goal = store
            .get_goal(&id)
            .expect("goal query")
            .expect("goal exists");
        assert_eq!(goal.priority, 0);
    }

    #[test]
    fn out_of_range_priority_is_rejected_instead_of_defaulted() {
        let mut store = migrated_store();
        let error = run_command(
            &mut store,
            "goal.create",
            &serde_json::json!({ "objective": "test", "priority": i64::from(i32::MAX) + 1 }),
            TEST_CALLER,
        )
        .expect_err("out-of-range priority must fail");

        assert!(error.contains("out of range"), "unexpected error: {error}");
        assert!(store.list_goals().expect("list goals").is_empty());
    }

    #[test]
    fn non_integer_priority_is_rejected() {
        let mut store = migrated_store();
        let error = run_command(
            &mut store,
            "goal.create",
            &serde_json::json!({ "objective": "test", "priority": "high" }),
            TEST_CALLER,
        )
        .expect_err("non-integer priority must fail");

        assert_eq!(error, "arg `priority` must be an integer");
        assert!(store.list_goals().expect("list goals").is_empty());
    }

    #[test]
    fn typed_goal_and_child_replay_keep_single_durable_effect() {
        let mut store = migrated_store();
        let create = serde_json::json!({
            "objective": "parent",
            "created_by": "uid:1000",
            "operation_id": "goal-once-parent"
        });
        let first =
            run_command(&mut store, "goal.create", &create, TEST_CALLER).expect("first goal");
        let replay =
            run_command(&mut store, "goal.create", &create, TEST_CALLER).expect("goal replay");
        assert_eq!(first, replay);

        let child = serde_json::json!({
            "id": first["id"],
            "objective": "child",
            "created_by": "uid:1000",
            "operation_id": "goal-once-child"
        });
        let first_child =
            run_command(&mut store, "goal.child", &child, TEST_CALLER).expect("first child");
        let replay_child =
            run_command(&mut store, "goal.child", &child, TEST_CALLER).expect("child replay");
        assert_eq!(first_child, replay_child);
        assert_eq!(store.list_goals().expect("goals").len(), 2);

        for id in [&first["id"], &first_child["id"]] {
            let id = GoalId::from_str(id.as_str().expect("id")).expect("goal id");
            let edges =
                CoreQueryRepository::provenance_from(&store, TEST_CALLER, &ObjectRef::from(&id))
                    .expect("provenance");
            assert!(!edges.is_empty(), "created object needs provenance");
        }
    }

    #[test]
    fn typed_goal_replay_rejects_changed_request() {
        let mut store = migrated_store();
        let original = serde_json::json!({
            "objective": "original",
            "created_by": "uid:1000",
            "operation_id": "goal-key-reused"
        });
        run_command(&mut store, "goal.create", &original, TEST_CALLER).expect("original");
        let changed = serde_json::json!({
            "objective": "changed",
            "created_by": "uid:1000",
            "operation_id": "goal-key-reused"
        });
        assert!(run_command(&mut store, "goal.create", &changed, TEST_CALLER).is_err());
        assert_eq!(store.list_goals().expect("goals").len(), 1);
    }

    #[test]
    fn child_requires_an_existing_parent_goal() {
        let mut store = migrated_store();
        let missing = GoalId::new();
        let error = run_command(
            &mut store,
            "goal.child",
            &serde_json::json!({ "id": missing, "objective": "child" }),
            TEST_CALLER,
        )
        .expect_err("missing parent must fail");

        assert_eq!(error, format!("goal not found: {missing}"));
        assert!(store.list_goals().expect("list goals").is_empty());
    }

    #[test]
    fn another_owners_goal_is_indistinguishable_from_a_missing_one() {
        let mut store = migrated_store();
        let mut theirs = Goal::new("theirs", "uid:2000");
        theirs.owner = Caller::new(2000).owner();
        store.insert_goal(&theirs).expect("insert goal");
        let missing = GoalId::new();
        for id in [theirs.id.clone(), missing] {
            for (command, args) in [
                ("goal.show", serde_json::json!({ "id": id })),
                ("goal.complete", serde_json::json!({ "id": id })),
                ("goal.abandon", serde_json::json!({ "id": id })),
                (
                    "goal.child",
                    serde_json::json!({ "id": id, "objective": "x" }),
                ),
            ] {
                let (code, message) =
                    validate_goal_command(&store, command, &args, TEST_CALLER).unwrap_err();
                assert_eq!(code, ErrorCode::NotFound, "{command}");
                assert_eq!(message, format!("goal not found: {id}"));
                let failure = run_command(&mut store, command, &args, TEST_CALLER).unwrap_err();
                assert_eq!(failure.code, ErrorCode::NotFound, "{command}");
            }
        }
        let listed =
            run_command(&mut store, "goal.list", &serde_json::json!({}), TEST_CALLER).unwrap();
        assert_eq!(listed["count"], 0);
        assert_eq!(
            store.get_goal(&theirs.id).unwrap().unwrap().status,
            theirs.status
        );
        assert_eq!(store.list_goals().unwrap().len(), 1, "no child was created");
    }

    #[test]
    fn child_of_existing_parent_is_created() {
        let mut store = migrated_store();
        let mut parent = Goal::new("parent", "test");
        parent.owner = TEST_CALLER.owner();
        store.insert_goal(&parent).expect("insert parent");

        let response = run_command(
            &mut store,
            "goal.child",
            &serde_json::json!({ "id": parent.id, "objective": "child" }),
            TEST_CALLER,
        )
        .expect("create child");

        assert_eq!(response["parent"], serde_json::json!(parent.id));
        let goals = store.list_goals().expect("list goals");
        assert_eq!(goals.len(), 2);
        assert!(goals
            .iter()
            .any(|goal| goal.parent_goal.as_ref() == Some(&parent.id)));
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
        let store = Arc::new(Mutex::new(migrated_store()));
        let goal = ipc(
            &store,
            "goal.create",
            serde_json::json!({"objective": "typed", "priority": null, "rationale": null, "future": 1}),
        )
        .expect("create with nulls and unknown field");
        let id = goal["id"].as_str().unwrap().to_owned();
        ipc(
            &store,
            "goal.child",
            serde_json::json!({"id": id, "objective": "child", "operation_id": null}),
        )
        .expect("child");
        ipc(
            &store,
            "goal.show",
            serde_json::json!({"id": id, "verbose": true}),
        )
        .expect("show");
        ipc(&store, "goal.list", serde_json::json!({})).expect("list");
        for (command, args) in [
            // Previously accepted with the rationale silently dropped.
            (
                "goal.create",
                serde_json::json!({"objective": "x", "rationale": 42}),
            ),
            (
                "goal.create",
                serde_json::json!({"objective": "x", "priority": 1.5}),
            ),
            (
                "goal.create",
                serde_json::json!({"objective": "x", "priority": 9_999_999_999_i64}),
            ),
            ("goal.create", serde_json::json!({"objective": 7})),
            (
                "goal.create",
                serde_json::json!({"objective": "x", "operation_id": 5}),
            ),
            ("goal.child", serde_json::json!({"id": id, "objective": 7})),
            ("goal.show", serde_json::json!({"id": 7})),
            ("goal.complete", serde_json::json!({})),
            ("goal.list", serde_json::json!(null)),
        ] {
            assert_eq!(
                ipc(&store, command, args.clone()),
                Err(ErrorCode::InvalidArgument),
                "{command} {args}"
            );
        }
        assert_eq!(
            ipc(
                &store,
                "goal.show",
                serde_json::json!({"id": "goal-00000000000000000000000000000000"})
            ),
            Err(ErrorCode::NotFound)
        );
    }
}
