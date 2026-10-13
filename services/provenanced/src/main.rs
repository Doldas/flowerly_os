use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use agentic_core::{
    coded_error_envelope, error_envelope, init_tracing, ok_envelope, run_service, ServiceConfig,
};
use agentic_ipc::{Envelope, ErrorCode, Payload};
use agentic_storage::{StorageError, Store};
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
    let db_path = config.database_path();
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
        Err(error) => coded_error_envelope(request.id.clone(), error.code, error.message),
    }
}

fn storage_error_code(error: &StorageError) -> ErrorCode {
    if error.is_busy() {
        ErrorCode::Unavailable
    } else {
        ErrorCode::Internal
    }
}

#[derive(Debug)]
struct CommandFailure {
    code: ErrorCode,
    message: String,
}

impl CommandFailure {
    fn storage(error: StorageError) -> Self {
        Self {
            code: storage_error_code(&error),
            message: error.to_string(),
        }
    }
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

impl PartialEq<String> for CommandFailure {
    fn eq(&self, other: &String) -> bool {
        &self.message == other
    }
}

fn run_command(store: &Store, name: &str, args: &Value) -> Result<Value, CommandFailure> {
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
                from: ObjectRef::from_str(from).map_err(err_str)?,
                relation,
                to: ObjectRef::from_str(to).map_err(err_str)?,
                created_at: Timestamp::now(),
                note: args.get("note").and_then(Value::as_str).map(str::to_owned),
            };
            store
                .insert_provenance(&edge)
                .map_err(CommandFailure::storage)?;
            tracing::info!(from, relation = %relation, to, "provenance edge added");
            Ok(serde_json::json!({ "from": from, "relation": relation.to_string(), "to": to }))
        }
        "provenance.from" => {
            let from = args
                .get("ref")
                .or_else(|| args.get("from"))
                .and_then(Value::as_str)
                .ok_or("missing string arg `ref`")?;
            let object_ref = ObjectRef::from_str(from).map_err(err_str)?;
            let edges = store
                .provenance_from(&object_ref)
                .map_err(CommandFailure::storage)?;
            Ok(edges_response(&edges))
        }
        "provenance.to" => {
            let to = args
                .get("ref")
                .or_else(|| args.get("to"))
                .and_then(Value::as_str)
                .ok_or("missing string arg `ref`")?;
            let object_ref = ObjectRef::from_str(to).map_err(err_str)?;
            let edges = store
                .provenance_to(&object_ref)
                .map_err(CommandFailure::storage)?;
            Ok(edges_response(&edges))
        }
        other => Err(format!("provenanced: unknown command `{other}`").into()),
    }
}

fn edges_response(edges: &[ProvenanceEdge]) -> Value {
    let items: Vec<Value> = edges
        .iter()
        .map(|edge| {
            serde_json::json!({
                "from": edge.from,
                "relation": edge.relation.to_string(),
                "to": edge.to,
                "note": edge.note,
            })
        })
        .collect();
    serde_json::json!({ "edges": items, "count": items.len() })
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
        from: ObjectRef::from_str("exec-123")?,
        relation: ProvenanceRelation::Produced,
        to: ObjectRef::from_str("artifact-456")?,
        created_at: Timestamp::now(),
        note: None,
    };
    store.insert_provenance(&edge)?;
    store.insert_provenance(&edge)?;
    let edges = store.provenance_from(&ObjectRef::from_str("exec-123")?)?;
    assert_eq!(edges.len(), 1);
    println!(
        "provenanced self-test ok: migrations={} edges={}",
        applied.len(),
        edges.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentic_types::{ExecutionId, GoalId};

    fn migrated_store() -> Store {
        let mut store = Store::open_in_memory().expect("open store");
        store.migrate().expect("migrate store");
        store
    }

    #[test]
    fn provenance_queries_work_in_both_directions() {
        let store = migrated_store();
        let execution = ExecutionId::new();
        let goal = GoalId::new();
        let edge = ProvenanceEdge {
            from: ObjectRef::from(&execution),
            relation: ProvenanceRelation::CausedBy,
            to: ObjectRef::from(&goal),
            created_at: Timestamp::now(),
            note: Some("test edge".to_owned()),
        };
        store.insert_provenance(&edge).expect("insert edge");

        let from = run_command(
            &store,
            "provenance.from",
            &serde_json::json!({ "ref": execution }),
        )
        .expect("query outgoing");
        assert_eq!(from["count"], 1);
        assert_eq!(from["edges"][0]["to"], serde_json::json!(goal));

        let to = run_command(&store, "provenance.to", &serde_json::json!({ "ref": goal }))
            .expect("query incoming");
        assert_eq!(to["count"], 1);
        assert_eq!(to["edges"][0]["from"], serde_json::json!(execution));
    }

    #[test]
    fn provenance_query_rejects_untyped_references() {
        let store = migrated_store();
        let error = run_command(
            &store,
            "provenance.to",
            &serde_json::json!({ "ref": "plain-string" }),
        )
        .expect_err("untyped reference must fail");
        assert!(
            error.contains("object reference"),
            "unexpected error: {error}"
        );
    }
}
