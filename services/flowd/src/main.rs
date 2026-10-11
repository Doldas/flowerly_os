//! `flowd` — persistent Flow contexts.
//!
//! A Flow is a coherent human activity context (`Development`, `Art`,
//! `Gaming`, ...). This service owns the Flow object lifecycle and starts with
//! a `General` flow so every installation has a safe default context.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use agentic_core::{error_envelope, init_tracing, ok_envelope, run_service, ServiceConfig};
use agentic_ipc::{Envelope, Payload};
use agentic_storage::Store;
use agentic_types::{
    Flow, FlowId, FlowState, ObjectRef, ProvenanceEdge, ProvenanceRelation, Timestamp,
};
use anyhow::Context;
use clap::Parser;
use serde_json::Value;

#[derive(Debug, Parser)]
#[command(name = "flowd", version, about = "Flowerly OS Flow service")]
struct Cli {
    /// Path to a TOML configuration file.
    #[arg(long, env = "AGENTIC_CONFIG")]
    config: Option<PathBuf>,
    /// Run a bounded self-test against a temporary database and exit.
    #[arg(long)]
    self_test: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = match cli.config {
        Some(path) => ServiceConfig::load(&path).context("loading config")?,
        None => ServiceConfig::for_service("flowd"),
    };
    init_tracing(&config.name, config.log_format);

    if cli.self_test {
        return self_test().await;
    }

    let store = open_store(&config)?;
    ensure_general_flow(&store)?;

    let extra = serde_json::json!({
        "flow_count": count_flows(&store)?,
        "default_flow": "General",
    });
    let store = Arc::new(Mutex::new(store));
    let handler = move |request: &Envelope| -> Envelope { handle_command(&store, request) };

    run_service(config, extra, Arc::new(handler))
        .await
        .context("flowd terminated abnormally")
}

fn open_store(config: &ServiceConfig) -> anyhow::Result<Store> {
    let db_path = config
        .database_path
        .clone()
        .unwrap_or_else(|| config.runtime_dir.join("flowd.db"));
    let mut store = Store::open(&db_path).context("opening flowd database")?;
    store.migrate().context("running flowd migrations")?;
    store
        .health_check()
        .context("flowd database health check")?;
    Ok(store)
}

fn ensure_general_flow(store: &Store) -> anyhow::Result<()> {
    if store.list_flows()?.iter().any(|f| f.name == "General") {
        return Ok(());
    }
    let mut general = Flow::new("General", "system");
    general.purpose = Some("The universal default context".to_owned());
    store.insert_flow(&general)?;
    store.insert_provenance(&ProvenanceEdge {
        from: ObjectRef::new(general.id.to_string()),
        relation: ProvenanceRelation::CreatedBy,
        to: ObjectRef::new("system".to_owned()),
        created_at: Timestamp::now(),
        note: Some("bootstrap default General flow".to_owned()),
    })?;
    tracing::info!(flow_id = %general.id, "created General flow");
    Ok(())
}

fn count_flows(store: &Store) -> anyhow::Result<usize> {
    Ok(store.list_flows()?.len())
}

fn handle_command(store: &Arc<Mutex<Store>>, request: &Envelope) -> Envelope {
    let Payload::Command { name, args } = &request.payload else {
        return error_envelope(request.id.clone(), "unexpected request type");
    };
    let store = match store.lock() {
        Ok(store) => store,
        Err(_) => {
            return error_envelope(request.id.clone(), "storage lock poisoned");
        }
    };
    let data = match run_command(&store, name, args) {
        Ok(data) => data,
        Err(message) => {
            return error_envelope(request.id.clone(), message);
        }
    };
    ok_envelope(request.id.clone(), data)
}

fn run_command(store: &Store, name: &str, args: &Value) -> std::result::Result<Value, String> {
    match name {
        "flow.create" => {
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or("missing string arg `name`")?;
            let created_by = args
                .get("created_by")
                .and_then(Value::as_str)
                .unwrap_or("user");
            let flow = Flow::new(name, created_by);
            store.insert_flow(&flow).map_err(err_str)?;
            store
                .insert_provenance(&ProvenanceEdge {
                    from: ObjectRef::new(flow.id.to_string()),
                    relation: ProvenanceRelation::CreatedBy,
                    to: ObjectRef::new(created_by.to_owned()),
                    created_at: Timestamp::now(),
                    note: None,
                })
                .map_err(err_str)?;
            tracing::info!(flow_id = %flow.id, flow_name = name, "flow created");
            Ok(serde_json::json!({ "id": flow.id, "name": flow.name, "state": flow.state }))
        }
        "flow.list" => {
            let flows = store.list_flows().map_err(err_str)?;
            let items: Vec<Value> = flows
                .iter()
                .map(|f| serde_json::json!({ "id": f.id, "name": f.name, "state": f.state }))
                .collect();
            Ok(serde_json::json!({ "flows": items, "count": items.len() }))
        }
        "flow.activate" | "flow.archive" => {
            let flow = find_flow(store, args)?;
            let state = if name == "flow.activate" {
                FlowState::Foreground
            } else {
                FlowState::Archived
            };
            store.set_flow_state(&flow.id, state).map_err(err_str)?;
            Ok(serde_json::json!({ "id": flow.id, "name": flow.name, "state": state }))
        }
        other => Err(format!("flowd: unknown command `{other}`")),
    }
}

fn find_flow(store: &Store, args: &Value) -> std::result::Result<agentic_types::Flow, String> {
    let by_id = args
        .get("id")
        .and_then(Value::as_str)
        .and_then(|raw| FlowId::from_str(raw).ok());
    if let Some(id) = by_id.clone() {
        if let Some(flow) = store.get_flow(&id).map_err(err_str)? {
            return Ok(flow);
        }
        return Err(format!("flow not found: {id}"));
    }
    if let Some(by_name) = args.get("name").and_then(Value::as_str) {
        let flows = store.list_flows().map_err(err_str)?;
        if let Some(flow) = flows.into_iter().find(|f| f.name == by_name) {
            return Ok(flow);
        }
        return Err(format!("flow not found: {by_name}"));
    }
    Err("provide either `id` or `name`".to_owned())
}

fn err_str(error: impl std::fmt::Display) -> String {
    error.to_string()
}

async fn self_test() -> anyhow::Result<()> {
    let dir = tempfile::tempdir().context("creating temp dir")?;
    let db = dir.path().join("flowd-self-test.db");
    let mut store = Store::open(&db).context("opening self-test db")?;
    let applied = store.migrate()?;
    ensure_general_flow(&store)?;
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
