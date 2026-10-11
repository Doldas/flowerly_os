use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use agentic_core::{error_envelope, init_tracing, ok_envelope, run_service, ServiceConfig};
use agentic_ipc::{Envelope, Payload};
use agentic_storage::Store;
use agentic_types::{ExperimentId, Generation, GenerationId, GenerationStatus, Timestamp};
use anyhow::Context;
use clap::Parser;
use serde_json::Value;

#[derive(Debug, Parser)]
#[command(
    name = "generationd",
    version,
    about = "Flowerly OS generation service"
)]
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
        None => ServiceConfig::for_service("generationd"),
    };
    init_tracing(&config.name, config.log_format);

    if cli.self_test {
        return self_test().await;
    }

    let store = open_store(&config)?;
    let extra = serde_json::json!({ "generation_count": store.list_generations()?.len() });
    let store = Arc::new(Mutex::new(store));
    let handler = move |request: &Envelope| -> Envelope { handle_command(&store, request) };

    run_service(config, extra, Arc::new(handler))
        .await
        .context("generationd terminated abnormally")
}

fn open_store(config: &ServiceConfig) -> anyhow::Result<Store> {
    let db_path = config
        .database_path
        .clone()
        .unwrap_or_else(|| config.runtime_dir.join("generationd.db"));
    let mut store = Store::open(&db_path).context("opening generationd database")?;
    store.migrate().context("running generationd migrations")?;
    store
        .health_check()
        .context("generationd database health check")?;
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
        "generation.create" => {
            let generation = Generation {
                id: GenerationId::new(),
                parent: args
                    .get("parent")
                    .and_then(Value::as_str)
                    .and_then(|raw| GenerationId::from_str(raw).ok()),
                source_commit: args
                    .get("source_commit")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                system_manifest: args
                    .get("system_manifest")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                reason_goal: None,
                experiment: args
                    .get("experiment")
                    .and_then(Value::as_str)
                    .map(ExperimentId::from_raw),
                status: GenerationStatus::Candidate,
                created_at: Timestamp::now(),
            };
            store.insert_generation(&generation).map_err(err_str)?;
            tracing::info!(generation_id = %generation.id, "generation created");
            Ok(serde_json::json!({
                "id": generation.id,
                "status": generation.status,
                "parent": generation.parent,
                "source_commit": generation.source_commit,
            }))
        }
        "generation.list" => {
            let items: Vec<Value> = store
                .list_generations()
                .map_err(err_str)?
                .iter()
                .map(|g| {
                    serde_json::json!({
                        "id": g.id,
                        "status": g.status,
                        "parent": g.parent,
                        "source_commit": g.source_commit,
                    })
                })
                .collect();
            Ok(serde_json::json!({ "generations": items, "count": items.len() }))
        }
        "generation.inspect" => {
            let id = args
                .get("id")
                .and_then(Value::as_str)
                .ok_or("missing string arg `id`")?;
            let generation = store
                .get_generation(&GenerationId::from_str(id).map_err(err_str)?)
                .map_err(err_str)?
                .ok_or_else(|| format!("generation not found: {id}"))?;
            Ok(serde_json::to_value(&generation).map_err(err_str)?)
        }
        "generation.activate" => {
            let id = args
                .get("id")
                .and_then(Value::as_str)
                .ok_or("missing string arg `id`")?;
            let generation_id = GenerationId::from_str(id).map_err(err_str)?;
            store
                .set_generation_status(&generation_id, GenerationStatus::Stable)
                .map_err(err_str)?;

            let generations = store.list_generations().map_err(err_str)?;
            for other in generations {
                if other.id != generation_id && matches!(other.status, GenerationStatus::Stable) {
                    store
                        .set_generation_status(&other.id, GenerationStatus::Superseded)
                        .map_err(err_str)?;
                }
            }
            Ok(serde_json::json!({ "id": generation_id, "status": GenerationStatus::Stable }))
        }
        other => Err(format!("generationd: unknown command `{other}`")),
    }
}

fn err_str(error: impl std::fmt::Display) -> String {
    error.to_string()
}

async fn self_test() -> anyhow::Result<()> {
    let dir = tempfile::tempdir().context("creating temp dir")?;
    let db = dir.path().join("generationd-self-test.db");
    let mut store = Store::open(&db).context("opening self-test db")?;
    let applied = store.migrate()?;
    let first = Generation {
        id: GenerationId::new(),
        parent: None,
        source_commit: Some("abc123".to_owned()),
        system_manifest: Some("sha256:deadbeef".to_owned()),
        reason_goal: None,
        experiment: None,
        status: GenerationStatus::Candidate,
        created_at: Timestamp::now(),
    };
    store.insert_generation(&first)?;
    let second = Generation {
        id: GenerationId::new(),
        parent: Some(first.id.clone()),
        source_commit: Some("def456".to_owned()),
        system_manifest: None,
        reason_goal: None,
        experiment: None,
        status: GenerationStatus::Candidate,
        created_at: Timestamp::now(),
    };
    store.insert_generation(&second)?;
    store.set_generation_status(&first.id, GenerationStatus::Stable)?;
    store.set_generation_status(&first.id, GenerationStatus::Superseded)?;
    assert_eq!(store.list_generations()?.len(), 2);
    println!("generationd self-test ok: migrations={}", applied.len());
    Ok(())
}
