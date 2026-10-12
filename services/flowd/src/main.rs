use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use agentic_core::{
    coded_error_envelope, error_envelope, init_tracing, ok_envelope, run_service_with_status,
    ServiceConfig,
};
use agentic_ipc::{Envelope, ErrorCode, Payload};
use agentic_storage::{StorageError, Store};
use agentic_types::{
    Flow, FlowId, FlowState, ObjectRef, ProvenanceEdge, ProvenanceRelation, Timestamp,
};
use anyhow::Context;
use clap::Parser;
use serde_json::Value;

#[derive(Debug, Parser)]
#[command(name = "flowd", version, about = "Flowerly OS Flow service")]
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
        None => ServiceConfig::for_service("flowd"),
    };
    init_tracing(&config.name, config.log_format);

    if cli.self_test {
        return self_test().await;
    }

    let mut store = open_store(&config)?;
    ensure_general_flow(&mut store)?;

    let store = Arc::new(Mutex::new(store));
    let status_store = Arc::clone(&store);
    let status = Arc::new(move || match status_store.lock() {
        Ok(store) => match count_flows(&store) {
            Ok(count) => {
                serde_json::json!({"flow_count": count, "ready": true, "default_flow": "General"})
            }
            Err(error) => serde_json::json!({"ready": false, "error": error.to_string()}),
        },
        Err(_) => serde_json::json!({"ready": false, "error": "storage lock poisoned"}),
    });
    let handler = move |request: &Envelope| -> Envelope { handle_command(&store, request) };

    run_service_with_status(config, status, Arc::new(handler))
        .await
        .context("flowd terminated abnormally")
}

fn open_store(config: &ServiceConfig) -> anyhow::Result<Store> {
    let db_path = config.database_path();
    let mut store = Store::open(&db_path).context("opening flowd database")?;
    store.migrate().context("running flowd migrations")?;
    store
        .health_check()
        .context("flowd database health check")?;
    Ok(store)
}

fn ensure_general_flow(store: &mut Store) -> anyhow::Result<()> {
    let id = store.ensure_general_flow()?;
    tracing::debug!(flow_id = %id, "General system Flow verified");
    Ok(())
}

fn count_flows(store: &Store) -> anyhow::Result<usize> {
    Ok(store.list_flows()?.len())
}

fn handle_command(store: &Arc<Mutex<Store>>, request: &Envelope) -> Envelope {
    let Payload::Command { name, args } = &request.payload else {
        return error_envelope(request.id.clone(), "unexpected request type");
    };
    let mut store = match store.lock() {
        Ok(store) => store,
        Err(_) => {
            return error_envelope(request.id.clone(), "storage lock poisoned");
        }
    };
    if let Err((code, message)) = validate_flow_command(&store, name, args) {
        return coded_error_envelope(request.id.clone(), code, message);
    }
    let data = match run_command(&mut store, name, args) {
        Ok(data) => data,
        Err(error) => {
            return coded_error_envelope(request.id.clone(), error.code, error.message);
        }
    };
    ok_envelope(request.id.clone(), data)
}

/// Classify user-facing Flow contract failures before mutation, while the
/// storage lock is held. Database errors are never inferred from text.
fn validate_flow_command(
    store: &Store,
    command: &str,
    args: &Value,
) -> Result<(), (ErrorCode, String)> {
    let invalid = |message: &str| (ErrorCode::InvalidArgument, message.to_owned());
    match command {
        "flow.create" => {
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("missing string arg `name`"))?;
            if name.trim().is_empty() {
                return Err(invalid("flow name must not be empty"));
            }
            if name == "General" {
                return Err((
                    ErrorCode::Conflict,
                    "General is reserved for the system Flow".to_owned(),
                ));
            }
        }
        "flow.list" => {}
        "flow.activate" | "flow.archive" => {
            let flow = if let Some(value) = args.get("id") {
                let raw = value
                    .as_str()
                    .ok_or_else(|| invalid("arg `id` must be a string"))?;
                let id = FlowId::from_str(raw).map_err(|e| invalid(&e.to_string()))?;
                store
                    .get_flow(&id)
                    .map_err(|e| (ErrorCode::Internal, e.to_string()))?
                    .ok_or_else(|| (ErrorCode::NotFound, format!("flow not found: {id}")))?
            } else if let Some(value) = args.get("name") {
                let name = value
                    .as_str()
                    .ok_or_else(|| invalid("arg `name` must be a string"))?;
                let mut matches = store
                    .list_flows()
                    .map_err(|e| (ErrorCode::Internal, e.to_string()))?
                    .into_iter()
                    .filter(|flow| flow.name == name);
                let found = matches
                    .next()
                    .ok_or_else(|| (ErrorCode::NotFound, format!("flow not found: {name}")))?;
                if matches.next().is_some() {
                    return Err((
                        ErrorCode::Conflict,
                        format!("ambiguous flow name: {name}; use id"),
                    ));
                }
                found
            } else {
                return Err(invalid("provide either `id` or `name`"));
            };
            if command == "flow.archive" && flow.name == "General" {
                return Err((
                    ErrorCode::PermissionDenied,
                    "General cannot be archived".to_owned(),
                ));
            }
            if command == "flow.activate" && flow.state == FlowState::Archived {
                return Err((ErrorCode::Conflict, format!("archived flow: {}", flow.id)));
            }
        }
        other => {
            return Err((
                ErrorCode::InvalidArgument,
                format!("flowd: unknown command `{other}`"),
            ));
        }
    }
    Ok(())
}

#[derive(Debug)]
struct CommandFailure {
    code: ErrorCode,
    message: String,
}

impl From<String> for CommandFailure {
    fn from(message: String) -> Self {
        Self { code: ErrorCode::Internal, message }
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
            code: if error.is_busy() { ErrorCode::Unavailable } else { ErrorCode::Internal },
            message: error.to_string(),
        }
    }
}

impl std::fmt::Display for CommandFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

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
        "flow.create" => {
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or("missing string arg `name`")?;
            let created_by = args
                .get("created_by")
                .and_then(Value::as_str)
                .unwrap_or("user");
            if name.trim().is_empty() {
                return Err("flow name must not be empty".into());
            }
            if name == "General" {
                return Err("General is reserved for the system Flow".into());
            }
            let flow = Flow::new(name, created_by);
            let provenance = ProvenanceEdge {
                from: ObjectRef::from(&flow.id),
                relation: ProvenanceRelation::CreatedBy,
                to: ObjectRef::actor(created_by).map_err(err_str)?,
                created_at: Timestamp::now(),
                note: None,
            };
            store
                .insert_flow_with_provenance(&flow, &[provenance])
                .map_err(CommandFailure::storage)?;
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
            if state == FlowState::Archived && flow.name == "General" {
                return Err("General cannot be archived".into());
            }
            if state == FlowState::Foreground {
                store.activate_flow(&flow.id).map_err(err_str)?;
            } else {
                store.set_flow_state(&flow.id, state).map_err(err_str)?;
            }
            Ok(serde_json::json!({ "id": flow.id, "name": flow.name, "state": state }))
        }
        other => Err(format!("flowd: unknown command `{other}`").into()),
    }
}

fn find_flow(store: &Store, args: &Value) -> std::result::Result<agentic_types::Flow, String> {
    if let Some(value) = args.get("id") {
        let raw = value.as_str().ok_or("arg `id` must be a string")?;
        let id = FlowId::from_str(raw).map_err(err_str)?;
        if let Some(flow) = store.get_flow(&id).map_err(err_str)? {
            return Ok(flow);
        }
        return Err(format!("flow not found: {id}"));
    }
    if let Some(by_name) = args.get("name").and_then(Value::as_str) {
        let flows = store.list_flows().map_err(err_str)?;
        let mut matches = flows.into_iter().filter(|f| f.name == by_name);
        if let Some(flow) = matches.next() {
            if matches.next().is_some() {
                return Err(format!("ambiguous flow name: {by_name}; use id"));
            }
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
    ensure_general_flow(&mut store)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn migrated_store() -> Store {
        let mut store = Store::open_in_memory().expect("open store");
        store.migrate().expect("migrate store");
        store
    }

    #[test]
    fn malformed_flow_id_is_rejected_instead_of_falling_back() {
        let store = migrated_store();
        let error = find_flow(&store, &serde_json::json!({ "id": "not-a-flow-id" }))
            .expect_err("invalid flow id must fail");

        assert!(
            error.contains("flow"),
            "unexpected invalid-id error: {error}"
        );
    }

    #[test]
    fn activation_demotes_previous_and_records_activity() {
        let mut store = migrated_store();
        let a = Flow::new("Alpha", "user");
        let b = Flow::new("Beta", "user");
        store.insert_flow(&a).unwrap();
        store.insert_flow(&b).unwrap();
        store.activate_flow(&a.id).unwrap();
        store.activate_flow(&b.id).unwrap();
        let old = store.get_flow(&a.id).unwrap().unwrap();
        let current = store.get_flow(&b.id).unwrap().unwrap();
        assert_eq!(old.state, FlowState::Active);
        assert_eq!(current.state, FlowState::Foreground);
        assert!(current.last_active.is_some());
        assert!(current.revision > b.revision);
        assert_eq!(
            store
                .list_flows()
                .unwrap()
                .iter()
                .filter(|f| f.state == FlowState::Foreground)
                .count(),
            1
        );
    }

    #[test]
    fn ambiguous_names_and_general_archive_are_rejected() {
        let mut store = migrated_store();
        ensure_general_flow(&mut store).unwrap();
        for _ in 0..2 {
            store.insert_flow(&Flow::new("Duplicate", "user")).unwrap();
        }
        assert!(find_flow(&store, &serde_json::json!({"name":"Duplicate"}))
            .unwrap_err()
            .contains("ambiguous"));
        assert!(run_command(
            &mut store,
            "flow.archive",
            &serde_json::json!({"name":"General"})
        )
        .unwrap_err()
        .contains("cannot be archived"));
    }

    #[test]
    fn flow_errors_have_explicit_codes() {
        let mut store = migrated_store();
        ensure_general_flow(&mut store).unwrap();
        let cases = [
            (
                "flow.create",
                serde_json::json!({"name": "General"}),
                ErrorCode::Conflict,
            ),
            (
                "flow.activate",
                serde_json::json!({"id": "flow-missing"}),
                ErrorCode::NotFound,
            ),
            (
                "flow.archive",
                serde_json::json!({"name": "General"}),
                ErrorCode::PermissionDenied,
            ),
            (
                "flow.activate",
                serde_json::json!({"id": 42}),
                ErrorCode::InvalidArgument,
            ),
        ];
        for (command, args, expected) in cases {
            assert_eq!(
                validate_flow_command(&store, command, &args).unwrap_err().0,
                expected
            );
        }
        for _ in 0..2 {
            store.insert_flow(&Flow::new("Collision", "user")).unwrap();
        }
        let ambiguous = validate_flow_command(
            &store,
            "flow.activate",
            &serde_json::json!({"name": "Collision"}),
        );
        assert_eq!(ambiguous.unwrap_err().0, ErrorCode::Conflict);
    }

    #[test]
    fn non_string_flow_id_is_rejected() {
        let store = migrated_store();
        let error = find_flow(&store, &serde_json::json!({ "id": 42 }))
            .expect_err("non-string flow id must fail");

        assert_eq!(error, "arg `id` must be a string");
    }
}
