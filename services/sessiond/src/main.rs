use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use agentic_core::{error_envelope, init_tracing, ok_envelope, run_service, ServiceConfig};
use agentic_ipc::{Envelope, Payload};
use agentic_storage::Store;
use agentic_types::{
    FlowId, ObjectRef, ProvenanceEdge, ProvenanceRelation, Session, SessionId, SessionState,
    Timestamp,
};
use anyhow::Context;
use clap::Parser;
use serde_json::Value;

#[derive(Debug, Parser)]
#[command(name = "sessiond", version, about = "Flowerly OS session service")]
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
        None => ServiceConfig::for_service("sessiond"),
    };
    init_tracing(&config.name, config.log_format);

    if cli.self_test {
        return self_test().await;
    }

    let store = open_store(&config)?;
    let extra = serde_json::json!({ "session_count": store.list_sessions()?.len() });
    let store = Arc::new(Mutex::new(store));
    let handler = move |request: &Envelope| -> Envelope { handle_command(&store, request) };

    run_service(config, extra, Arc::new(handler))
        .await
        .context("sessiond terminated abnormally")
}

fn open_store(config: &ServiceConfig) -> anyhow::Result<Store> {
    let db_path = config.database_path();
    let mut store = Store::open(&db_path).context("opening sessiond database")?;
    store.migrate().context("running sessiond migrations")?;
    store
        .health_check()
        .context("sessiond database health check")?;
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
        "session.create" => {
            let kind = args
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("local-pty");
            let created_by = args
                .get("created_by")
                .and_then(Value::as_str)
                .unwrap_or("user");
            let mut session = Session {
                id: SessionId::new(),
                flow_id: args
                    .get("flow_id")
                    .and_then(Value::as_str)
                    .and_then(|raw| FlowId::from_str(raw).ok()),
                kind: kind.to_owned(),
                pid: args.get("pid").and_then(Value::as_i64).map(|v| v as i32),
                cwd: args.get("cwd").and_then(Value::as_str).map(str::to_owned),
                state: SessionState::Running,
                created_by: created_by.to_owned(),
                created_at: Timestamp::now(),
            };
            if session.pid.is_none() {
                session.state = SessionState::Detached;
            }
            store.insert_session(&session).map_err(err_str)?;
            store
                .insert_provenance(&ProvenanceEdge {
                    from: ObjectRef::new(session.id.to_string()),
                    relation: ProvenanceRelation::CreatedBy,
                    to: ObjectRef::new(created_by.to_owned()),
                    created_at: Timestamp::now(),
                    note: None,
                })
                .map_err(err_str)?;
            tracing::info!(session_id = %session.id, kind, "session created");
            Ok(serde_json::json!({
                "id": session.id,
                "kind": session.kind,
                "state": session.state,
                "flow_id": session.flow_id,
            }))
        }
        "session.list" => {
            let sessions = store.list_sessions().map_err(err_str)?;
            let items: Vec<Value> = sessions
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "id": s.id,
                        "kind": s.kind,
                        "state": s.state,
                        "flow_id": s.flow_id,
                        "pid": s.pid,
                        "created_by": s.created_by,
                    })
                })
                .collect();
            Ok(serde_json::json!({ "sessions": items, "count": items.len() }))
        }
        "session.terminate" => {
            let id = args
                .get("id")
                .and_then(Value::as_str)
                .ok_or("missing string arg `id`")?;
            let session_id = SessionId::from_str(id).map_err(err_str)?;
            let session = store
                .get_session(&session_id)
                .map_err(err_str)?
                .ok_or_else(|| format!("session not found: {id}"))?;

            store
                .update_session_state(&session_id, SessionState::Exited)
                .map_err(err_str)?;
            Ok(
                serde_json::json!({ "id": session_id, "state": SessionState::Exited, "was": session.state }),
            )
        }
        other => Err(format!("sessiond: unknown command `{other}`")),
    }
}

fn err_str(error: impl std::fmt::Display) -> String {
    error.to_string()
}

async fn self_test() -> anyhow::Result<()> {
    let dir = tempfile::tempdir().context("creating temp dir")?;
    let db = dir.path().join("sessiond-self-test.db");
    let mut store = Store::open(&db).context("opening self-test db")?;
    let applied = store.migrate()?;
    let session = Session {
        id: SessionId::new(),
        flow_id: None,
        kind: "local-pty".to_owned(),
        pid: Some(42),
        cwd: Some("/home".to_owned()),
        state: SessionState::Running,
        created_by: "self-test".to_owned(),
        created_at: Timestamp::now(),
    };
    store.insert_session(&session)?;
    store.update_session_state(&session.id, SessionState::Exited)?;
    assert_eq!(store.list_sessions()?.len(), 1);
    println!("sessiond self-test ok: migrations={}", applied.len());
    Ok(())
}
