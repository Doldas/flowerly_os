use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use agentic_core::{error_envelope, init_tracing, ok_envelope, run_service, ServiceConfig};
use agentic_ipc::{Envelope, Payload};
use agentic_storage::Store;
use agentic_types::{
    Goal, GoalId, GoalStatus, ObjectRef, ProvenanceEdge, ProvenanceRelation, Timestamp,
};
use anyhow::Context;
use clap::Parser;
use serde_json::Value;

#[derive(Debug, Parser)]
#[command(name = "goald", version, about = "Flowerly OS goal service")]
struct Cli {
    #[arg(long, env = "AGENTIC_CONFIG")]
    config: Option<PathBuf>,

    #[arg(long)]
    self_test: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = match cli.config {
        Some(path) => ServiceConfig::load(&path).context("loading config")?,
        None => ServiceConfig::for_service("goald"),
    };
    init_tracing(&config.name, config.log_format);

    if cli.self_test {
        return self_test().await;
    }

    let store = open_store(&config)?;
    let extra = serde_json::json!({ "goal_count": store.list_goals()?.len() });
    let store = Arc::new(Mutex::new(store));
    let handler = move |request: &Envelope| -> Envelope { handle_command(&store, request) };

    run_service(config, extra, Arc::new(handler))
        .await
        .context("goald terminated abnormally")
}

fn open_store(config: &ServiceConfig) -> anyhow::Result<Store> {
    let db_path = config.database_path();
    let mut store = Store::open(&db_path).context("opening goald database")?;
    store.migrate().context("running goald migrations")?;
    store
        .health_check()
        .context("goald database health check")?;
    Ok(store)
}

fn handle_command(store: &Arc<Mutex<Store>>, request: &Envelope) -> Envelope {
    let Payload::Command { name, args } = &request.payload else {
        return error_envelope(request.id.clone(), "unexpected request type");
    };
    let mut store = match store.lock() {
        Ok(store) => store,
        Err(_) => return error_envelope(request.id.clone(), "storage lock poisoned"),
    };
    match run_command(&mut store, name, args) {
        Ok(data) => ok_envelope(request.id.clone(), data),
        Err(message) => error_envelope(request.id.clone(), message),
    }
}

fn run_command(store: &mut Store, name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "goal.create" => {
            let objective = args
                .get("objective")
                .and_then(Value::as_str)
                .ok_or("missing string arg `objective`")?;
            let created_by = args
                .get("created_by")
                .and_then(Value::as_str)
                .unwrap_or("user");
            let mut goal = Goal::new(objective, created_by);
            if let Some(value) = args.get("priority") {
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
            store
                .insert_goal_with_provenance(&goal, &[created_edge])
                .map_err(err_str)?;
            tracing::info!(goal_id = %goal.id, objective, "goal created");
            Ok(serde_json::json!({
                "id": goal.id,
                "objective": goal.objective,
                "status": goal.status,
                "parent_goal": goal.parent_goal,
            }))
        }
        "goal.child" => {
            let parent_id = args
                .get("id")
                .and_then(Value::as_str)
                .ok_or("missing string arg `id`")?;
            let parent = GoalId::from_str(parent_id).map_err(err_str)?;
            if store.get_goal(&parent).map_err(err_str)?.is_none() {
                return Err(format!("goal not found: {parent}"));
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
            store
                .insert_goal_with_provenance(&child, &[created_edge, parent_edge])
                .map_err(err_str)?;
            Ok(
                serde_json::json!({ "id": child.id, "objective": child.objective, "parent": parent }),
            )
        }
        "goal.show" => {
            let id = args
                .get("id")
                .and_then(Value::as_str)
                .ok_or("missing string arg `id`")?;
            let goal = store
                .get_goal(&GoalId::from_str(id).map_err(err_str)?)
                .map_err(err_str)?
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
            let goals = store.list_goals().map_err(err_str)?;
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
            store.set_goal_status(&goal_id, status).map_err(err_str)?;
            Ok(serde_json::json!({ "id": goal_id, "status": status }))
        }
        other => Err(format!("goald: unknown command `{other}`")),
    }
}

fn err_str(error: impl std::fmt::Display) -> String {
    error.to_string()
}

async fn self_test() -> anyhow::Result<()> {
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
