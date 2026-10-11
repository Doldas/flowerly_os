use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agentic_core::{error_envelope, init_tracing, ok_envelope, run_service, ServiceConfig};
use agentic_ipc::{Envelope, Payload};
use agentic_storage::Store;
use agentic_types::{ObjectRef, ProvenanceEdge, ProvenanceRelation, Timestamp};
use anyhow::Context;
use clap::Parser;
use serde_json::Value;

#[derive(Debug, Parser)]
#[command(
    name = "provenanced",
    version,
    about = "Flowerly OS provenance service"
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
        None => ServiceConfig::for_service("provenanced"),
    };
    init_tracing(&config.name, config.log_format);

    if cli.self_test {
        return self_test().await;
    }

    let store = open_store(&config)?;
    let extra = serde_json::json!({ "edge_count": 0 });
    let store = Arc::new(Mutex::new(store));
    let handler = move |request: &Envelope| -> Envelope { handle_command(&store, request) };

    run_service(config, extra, Arc::new(handler))
        .await
        .context("provenanced terminated abnormally")
}

fn open_store(config: &ServiceConfig) -> anyhow::Result<Store> {
    let db_path = config
        .database_path
        .clone()
        .unwrap_or_else(|| config.runtime_dir.join("provenanced.db"));
    let mut store = Store::open(&db_path).context("opening provenanced database")?;
    store.migrate().context("running provenanced migrations")?;
    store
        .health_check()
        .context("provenanced database health check")?;
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
        "provenance.add" => {
            let from = args
                .get("from")
                .and_then(Value::as_str)
                .ok_or("missing string arg `from`")?;
            let relation = args
                .get("relation")
                .and_then(Value::as_str)
                .ok_or("missing string arg `relation`")?;
            let to = args
                .get("to")
                .and_then(Value::as_str)
                .ok_or("missing string arg `to`")?;
            let relation = parse_relation(relation)?;
            let edge = ProvenanceEdge {
                from: ObjectRef::new(from.to_owned()),
                relation,
                to: ObjectRef::new(to.to_owned()),
                created_at: Timestamp::now(),
                note: args.get("note").and_then(Value::as_str).map(str::to_owned),
            };
            store.insert_provenance(&edge).map_err(err_str)?;
            tracing::info!(from, relation = %relation, to, "provenance edge added");
            Ok(serde_json::json!({ "from": from, "relation": relation.to_string(), "to": to }))
        }
        "provenance.from" => {
            let from = args
                .get("ref")
                .or_else(|| args.get("from"))
                .and_then(Value::as_str)
                .ok_or("missing string arg `ref`")?;
            let edges = store
                .provenance_from(&ObjectRef::new(from.to_owned()))
                .map_err(err_str)?;
            let items: Vec<Value> = edges
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "from": e.from,
                        "relation": e.relation.to_string(),
                        "to": e.to,
                        "note": e.note,
                    })
                })
                .collect();
            Ok(serde_json::json!({ "edges": items, "count": items.len() }))
        }
        other => Err(format!("provenanced: unknown command `{other}`")),
    }
}

fn parse_relation(raw: &str) -> Result<ProvenanceRelation, String> {
    match raw {
        "created_by" => Ok(ProvenanceRelation::CreatedBy),
        "caused_by" => Ok(ProvenanceRelation::CausedBy),
        "belongs_to" => Ok(ProvenanceRelation::BelongsTo),
        "produced" => Ok(ProvenanceRelation::Produced),
        "tested_by" => Ok(ProvenanceRelation::TestedBy),
        "promoted_to" => Ok(ProvenanceRelation::PromotedTo),
        other => Err(format!("unknown relation `{other}`")),
    }
}

fn err_str(error: impl std::fmt::Display) -> String {
    error.to_string()
}

async fn self_test() -> anyhow::Result<()> {
    let dir = tempfile::tempdir().context("creating temp dir")?;
    let db = dir.path().join("provenanced-self-test.db");
    let mut store = Store::open(&db).context("opening self-test db")?;
    let applied = store.migrate()?;
    let edge = ProvenanceEdge {
        from: ObjectRef::new("exec-123".to_owned()),
        relation: ProvenanceRelation::Produced,
        to: ObjectRef::new("artifact-456".to_owned()),
        created_at: Timestamp::now(),
        note: None,
    };
    store.insert_provenance(&edge)?;
    store.insert_provenance(&edge)?;
    let edges = store.provenance_from(&ObjectRef::new("exec-123".to_owned()))?;
    assert_eq!(edges.len(), 1);
    println!(
        "provenanced self-test ok: migrations={} edges={}",
        applied.len(),
        edges.len()
    );
    Ok(())
}
