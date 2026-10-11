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
    let store = match store.lock() {
        Ok(store) => store,
        Err(_) => return error_envelope(request.id.clone(), "storage lock poisoned"),
    };
    match run_command(&store, name, args) {
        Ok(data) => ok_envelope(request.id.clone(), data),
        Err(message) => error_envelope(request.id.clone(), message),
    }
}

fn run_command(store: &Store, name: &str, args: &Value) -> Result<Value, String> {
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
            if let Some(priority) = args.get("priority").and_then(Value::as_i64) {
                goal.priority = i32::try_from(priority).unwrap_or_default();
            }
            goal.rationale = args
                .get("rationale")
                .and_then(Value::as_str)
                .map(str::to_owned);
            store.insert_goal(&goal).map_err(err_str)?;
            link_created(store, &goal.id, created_by)?;
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
            store.insert_goal(&child).map_err(err_str)?;
            store
                .insert_provenance(&ProvenanceEdge {
                    from: ObjectRef::new(child.id.to_string()),
                    relation: ProvenanceRelation::BelongsTo,
                    to: ObjectRef::new(parent.to_string()),
                    created_at: Timestamp::now(),
                    note: Some("child goal".to_owned()),
                })
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

fn link_created(store: &Store, goal_id: &GoalId, actor: &str) -> Result<(), String> {
    store
        .insert_provenance(&ProvenanceEdge {
            from: ObjectRef::new(goal_id.to_string()),
            relation: ProvenanceRelation::CreatedBy,
            to: ObjectRef::new(actor.to_owned()),
            created_at: Timestamp::now(),
            note: None,
        })
        .map_err(err_str)
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
