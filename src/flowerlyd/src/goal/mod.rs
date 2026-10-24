use std::str::FromStr;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use flowerly_core::ipc::{Envelope, ErrorCode, Payload};
use flowerly_core::storage::repository::{CoreCommandRepository, CoreQueryRepository, CreateGoal};
use flowerly_core::storage::{MutationOperation, StorageError, Store};
use flowerly_core::types::{
    Goal, GoalId, GoalStatus, ObjectRef, ProvenanceEdge, ProvenanceRelation, Timestamp,
};
use flowerly_core::{coded_error_envelope, error_envelope, ok_envelope};
use serde_json::Value;

pub(crate) fn handle_command(store: &Arc<Mutex<Store>>, request: &Envelope) -> Envelope {
    let Payload::Command { name, args } = &request.payload else {
        return error_envelope(request.id.clone(), "unexpected request type");
    };
    let mut store = match store.lock() {
        Ok(store) => store,
        Err(_) => return error_envelope(request.id.clone(), "storage lock poisoned"),
    };
    if let Err((code, message)) = validate_goal_command(&store, name, args) {
        return coded_error_envelope(request.id.clone(), code, message);
    }
    match run_command(&mut store, name, args) {
        Ok(data) => ok_envelope(request.id.clone(), data),
        Err(error) => coded_error_envelope(request.id.clone(), error.code, error.message),
    }
}

/// Validate client-facing Goal errors while the store lock is held.
/// SQLite errors remain internal instead of being inferred from error messages.
fn validate_goal_command(
    store: &Store,
    name: &str,
    args: &Value,
) -> Result<(), (ErrorCode, String)> {
    let invalid = |message: &str| (ErrorCode::InvalidArgument, message.to_owned());
    match name {
        "goal.create" | "goal.child" => {
            if args.get("objective").and_then(Value::as_str).is_none() {
                return Err(invalid("missing string arg `objective`"));
            }
            if name == "goal.create" {
                if let Some(value) = args.get("priority").filter(|value| !value.is_null()) {
                    let raw = value
                        .as_i64()
                        .ok_or_else(|| invalid("arg `priority` must be an integer"))?;
                    i32::try_from(raw).map_err(|_| invalid("priority out of range for i32"))?;
                }
            }
        }
        "goal.list" => return Ok(()),
        "goal.show" | "goal.complete" | "goal.abandon" => {}
        other => {
            return Err((
                ErrorCode::InvalidArgument,
                format!("goald: unknown command `{other}`"),
            ));
        }
    }
    if matches!(
        name,
        "goal.child" | "goal.show" | "goal.complete" | "goal.abandon"
    ) {
        let raw = args
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("missing string arg `id`"))?;
        let id = GoalId::from_str(raw).map_err(|error| invalid(&error.to_string()))?;
        if CoreQueryRepository::goal(store, &id)
            .map_err(|error| (storage_error_code(&error), error.to_string()))?
            .is_none()
        {
            return Err((ErrorCode::NotFound, format!("goal not found: {raw}")));
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

fn run_command(store: &mut Store, name: &str, args: &Value) -> Result<Value, CommandFailure> {
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
            if CoreQueryRepository::goal(store, &parent)
                .map_err(CommandFailure::storage)?
                .is_none()
            {
                return Err(format!("goal not found: {parent}").into());
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
            let goal = CoreQueryRepository::goal(store, &GoalId::from_str(id).map_err(err_str)?)
                .map_err(CommandFailure::storage)?
                .ok_or_else(|| format!("goal not found: {id}"))?;
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
            let goals = CoreQueryRepository::goals(store).map_err(CommandFailure::storage)?;
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
            store
                .set_goal_status(&goal_id, status)
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
                validate_goal_command(&store, command, &args).unwrap_err().0,
                expected
            );
        }
        assert!(validate_goal_command(
            &store,
            "goal.create",
            &serde_json::json!({"objective": "valid", "priority": null}),
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
        let first = run_command(&mut store, "goal.create", &create).expect("first goal");
        let replay = run_command(&mut store, "goal.create", &create).expect("goal replay");
        assert_eq!(first, replay);

        let child = serde_json::json!({
            "id": first["id"],
            "objective": "child",
            "created_by": "uid:1000",
            "operation_id": "goal-once-child"
        });
        let first_child = run_command(&mut store, "goal.child", &child).expect("first child");
        let replay_child = run_command(&mut store, "goal.child", &child).expect("child replay");
        assert_eq!(first_child, replay_child);
        assert_eq!(store.list_goals().expect("goals").len(), 2);

        for id in [&first["id"], &first_child["id"]] {
            let id = GoalId::from_str(id.as_str().expect("id")).expect("goal id");
            let edges = CoreQueryRepository::provenance_from(&store, &ObjectRef::from(&id))
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
        run_command(&mut store, "goal.create", &original).expect("original");
        let changed = serde_json::json!({
            "objective": "changed",
            "created_by": "uid:1000",
            "operation_id": "goal-key-reused"
        });
        assert!(run_command(&mut store, "goal.create", &changed).is_err());
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
        )
        .expect_err("missing parent must fail");

        assert_eq!(error, format!("goal not found: {missing}"));
        assert!(store.list_goals().expect("list goals").is_empty());
    }

    #[test]
    fn child_of_existing_parent_is_created() {
        let mut store = migrated_store();
        let parent = Goal::new("parent", "test");
        store.insert_goal(&parent).expect("insert parent");

        let response = run_command(
            &mut store,
            "goal.child",
            &serde_json::json!({ "id": parent.id, "objective": "child" }),
        )
        .expect("create child");

        assert_eq!(response["parent"], serde_json::json!(parent.id));
        let goals = store.list_goals().expect("list goals");
        assert_eq!(goals.len(), 2);
        assert!(goals
            .iter()
            .any(|goal| goal.parent_goal.as_ref() == Some(&parent.id)));
    }
}
