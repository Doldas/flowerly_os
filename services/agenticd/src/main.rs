use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use agentic_core::{error_envelope, init_tracing, ok_envelope, run_service, ServiceConfig};
use agentic_ipc::{Envelope, Payload};
use agentic_storage::Store;
use agentic_types::{AgentId, Execution, ExecutionId, GoalId, Timestamp};
use anyhow::Context;
use clap::Parser;
use serde_json::Value;

#[derive(Debug, Parser)]
#[command(name = "agenticd", version, about = "Flowerly OS orchestration daemon")]
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
        None => ServiceConfig::for_service("agenticd"),
    };
    init_tracing(&config.name, config.log_format);

    if cli.self_test {
        return self_test().await;
    }

    let store = open_store(&config)?;
    let extra = serde_json::json!({ "role": "orchestrator", "agents_started": 0 });
    let store = Arc::new(Mutex::new(store));
    let handler = move |request: &Envelope| -> Envelope { handle_command(&store, request) };

    run_service(config, extra, Arc::new(handler))
        .await
        .context("agenticd terminated abnormally")
}

fn open_store(config: &ServiceConfig) -> anyhow::Result<Store> {
    let db_path = config.database_path();
    let mut store = Store::open(&db_path).context("opening agenticd database")?;
    store.migrate().context("running agenticd migrations")?;
    store
        .health_check()
        .context("agenticd database health check")?;
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
        "execution.create" => {
            let exec = Execution {
                id: ExecutionId::new(),
                agent_id: args
                    .get("agent_id")
                    .and_then(Value::as_str)
                    .and_then(|raw| AgentId::from_str(raw).ok()),
                goal_id: args
                    .get("goal_id")
                    .and_then(Value::as_str)
                    .and_then(|raw| GoalId::from_str(raw).ok()),
                reason: args
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                command: args
                    .get("command")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                exit_code: args
                    .get("exit_code")
                    .and_then(Value::as_i64)
                    .map(|code| code as i32),
                duration_ms: args.get("duration_ms").and_then(Value::as_i64),
                started_at: Timestamp::now(),
            };
            store.insert_execution(&exec).map_err(err_str)?;
            tracing::info!(
                execution_id = %exec.id,
                agent_id = ?exec.agent_id,
                goal_id = ?exec.goal_id,
                "execution recorded"
            );
            Ok(serde_json::json!({ "id": exec.id }))
        }
        "execution.show" => {
            let raw = args
                .get("id")
                .and_then(Value::as_str)
                .ok_or("missing `id`")?;
            let exec_id = ExecutionId::from_str(raw).map_err(err_str)?;
            let record = store.get_execution(&exec_id).map_err(err_str)?;
            let data = match record {
                Some(record) => serde_json::to_value(&record).map_err(err_str)?,
                None => serde_json::json!(null),
            };
            Ok(serde_json::json!({ "execution": data }))
        }
        "execution.list" => {
            let records = store.list_executions().map_err(err_str)?;
            Ok(serde_json::json!({ "executions": records, "count": records.len() }))
        }
        other => Err(format!("agenticd: unknown command `{other}`")),
    }
}

fn err_str(error: impl std::fmt::Display) -> String {
    error.to_string()
}

async fn self_test() -> anyhow::Result<()> {
    let dir = tempfile::tempdir().context("creating temp dir")?;
    let db = dir.path().join("agenticd-self-test.db");
    let mut store = Store::open(&db).context("opening self-test db")?;
    let applied = store.migrate()?;
    let execution = Execution {
        id: ExecutionId::new(),
        agent_id: Some(AgentId::new()),
        goal_id: Some(GoalId::new()),
        reason: Some("self-test".to_owned()),
        command: Some("cargo test".to_owned()),
        exit_code: Some(0),
        duration_ms: Some(1),
        started_at: Timestamp::now(),
    };
    store.insert_execution(&execution)?;
    let loaded = store
        .get_execution(&execution.id)?
        .expect("execution persisted");
    assert_eq!(loaded.exit_code, Some(0));
    println!("agenticd self-test ok: migrations={}", applied.len());
    Ok(())
}
