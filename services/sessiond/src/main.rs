use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use agentic_core::{
    coded_error_envelope, error_envelope, init_tracing, ok_envelope, run_service_with_status,
    ServiceConfig,
};
use agentic_ipc::{Envelope, ErrorCode, Payload};
use agentic_storage::Store;
use agentic_types::{
    FlowId, ObjectRef, ProvenanceEdge, ProvenanceRelation, Session, SessionId,
    SessionPresentationState, SessionState, Timestamp,
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
    let store = Arc::new(Mutex::new(store));
    let status_store = Arc::clone(&store);
    let status = Arc::new(move || match status_store.try_lock() {
        Ok(store) => match store.list_sessions().map(|items| items.len()) {
            Ok(count) => serde_json::json!({"session_count": count, "ready": true}),
            Err(error) => serde_json::json!({"ready": false, "error": error.to_string()}),
        },
        Err(_) => serde_json::json!({"ready": false, "error": "storage temporarily unavailable or lock poisoned"}),
    });
    let handler = move |request: &Envelope| -> Envelope { handle_command(&store, request) };

    run_service_with_status(config, status, Arc::new(handler))
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
    let mut store = match store.lock() {
        Ok(store) => store,
        Err(_) => return error_envelope(request.id.clone(), "storage lock poisoned"),
    };
    if let Err((code, message)) = validate_session_command(&store, name, args) {
        return coded_error_envelope(request.id.clone(), code, message);
    }
    match run_command(&mut store, name, args) {
        Ok(data) => ok_envelope(request.id.clone(), data),
        Err(message) => error_envelope(request.id.clone(), message),
    }
}

/// Validate API errors under the same storage mutex used for the mutation.
/// Storage failures remain internal; no string-based classification.
fn validate_session_command(
    store: &Store,
    name: &str,
    args: &Value,
) -> Result<(), (ErrorCode, String)> {
    let invalid = |message: &str| (ErrorCode::InvalidArgument, message.to_owned());
    match name {
        "session.create" => {
            if let Some(value) = args.get("flow_id").filter(|value| !value.is_null()) {
                let raw = value
                    .as_str()
                    .ok_or_else(|| invalid("arg `flow_id` must be a string or null"))?;
                let flow_id = FlowId::from_str(raw).map_err(|e| invalid(&e.to_string()))?;
                if store
                    .get_flow(&flow_id)
                    .map_err(|e| (ErrorCode::Internal, e.to_string()))?
                    .is_none()
                {
                    return Err((ErrorCode::NotFound, format!("flow not found: {flow_id}")));
                }
            }
            if let Some(value) = args.get("pid").filter(|value| !value.is_null()) {
                let raw = value
                    .as_i64()
                    .ok_or_else(|| invalid("arg `pid` must be an integer or null"))?;
                i32::try_from(raw).map_err(|_| invalid("pid out of range for i32"))?;
            }
        }
        "session.list" => {}
        "session.terminate" => {
            let raw = args
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("missing string arg `id`"))?;
            let id = SessionId::from_str(raw).map_err(|e| invalid(&e.to_string()))?;
            if store
                .get_session(&id)
                .map_err(|e| (ErrorCode::Internal, e.to_string()))?
                .is_none()
            {
                return Err((ErrorCode::NotFound, format!("session not found: {raw}")));
            }
        }
        other => {
            return Err((
                ErrorCode::InvalidArgument,
                format!("sessiond: unknown command `{other}`"),
            ));
        }
    }
    Ok(())
}

fn run_command(store: &mut Store, name: &str, args: &Value) -> Result<Value, String> {
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
            let flow_id = parse_optional_flow_id(store, args)?;
            let pid = parse_optional_pid(args)?;
            let session = Session {
                id: SessionId::new(),
                flow_id,
                kind: kind.to_owned(),
                pid,
                cwd: args.get("cwd").and_then(Value::as_str).map(str::to_owned),
                state: SessionState::Running,
                presentation_state: if pid.is_some() {
                    SessionPresentationState::Attached
                } else {
                    SessionPresentationState::Detached
                },
                created_by: created_by.to_owned(),
                created_at: Timestamp::now(),
            };
            let provenance = ProvenanceEdge {
                from: ObjectRef::from(&session.id),
                relation: ProvenanceRelation::CreatedBy,
                to: ObjectRef::actor(created_by).map_err(err_str)?,
                created_at: Timestamp::now(),
                note: None,
            };
            store
                .insert_session_with_provenance(&session, &[provenance])
                .map_err(err_str)?;
            tracing::info!(session_id = %session.id, kind, "session created");
            Ok(serde_json::json!({
                "id": session.id,
                "kind": session.kind,
                "state": session.state,
                "presentation_state": session.presentation_state,
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
                        "presentation_state": s.presentation_state,
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
            store
                .update_session_presentation_state(&session_id, SessionPresentationState::Detached)
                .map_err(err_str)?;
            Ok(serde_json::json!({
                "id": session_id,
                "state": SessionState::Exited,
                "presentation_state": SessionPresentationState::Detached,
                "was": session.state,
                "presentation_was": session.presentation_state,
            }))
        }
        other => Err(format!("sessiond: unknown command `{other}`")),
    }
}

fn parse_optional_flow_id(store: &Store, args: &Value) -> Result<Option<FlowId>, String> {
    let Some(value) = args.get("flow_id") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }

    let raw = value
        .as_str()
        .ok_or("arg `flow_id` must be a string or null")?;
    let flow_id = FlowId::from_str(raw).map_err(err_str)?;
    if store.get_flow(&flow_id).map_err(err_str)?.is_none() {
        return Err(format!("flow not found: {flow_id}"));
    }
    Ok(Some(flow_id))
}

fn parse_optional_pid(args: &Value) -> Result<Option<i32>, String> {
    let Some(value) = args.get("pid") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }

    let raw = value
        .as_i64()
        .ok_or("arg `pid` must be an integer or null")?;
    let pid = i32::try_from(raw).map_err(|_| format!("pid out of range for i32: {raw}"))?;
    Ok(Some(pid))
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
        presentation_state: SessionPresentationState::Attached,
        created_by: "self-test".to_owned(),
        created_at: Timestamp::now(),
    };
    store.insert_session(&session)?;
    store.update_session_state(&session.id, SessionState::Exited)?;
    assert_eq!(store.list_sessions()?.len(), 1);
    println!("sessiond self-test ok: migrations={}", applied.len());
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
    fn session_command_validation_returns_explicit_codes() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let cases = [
            (
                "session.create",
                serde_json::json!({"flow_id": 42}),
                ErrorCode::InvalidArgument,
            ),
            (
                "session.create",
                serde_json::json!({"pid": 9223372036854775807i64}),
                ErrorCode::InvalidArgument,
            ),
            (
                "session.create",
                serde_json::json!({"flow_id": "flow-missing"}),
                ErrorCode::NotFound,
            ),
            (
                "session.terminate",
                serde_json::json!({"id": "session-missing"}),
                ErrorCode::NotFound,
            ),
            (
                "session.terminate",
                serde_json::json!({"id": 42}),
                ErrorCode::InvalidArgument,
            ),
            ("unknown", serde_json::json!({}), ErrorCode::InvalidArgument),
        ];
        for (command, args, expected) in cases {
            assert_eq!(
                validate_session_command(&store, command, &args)
                    .unwrap_err()
                    .0,
                expected
            );
        }
    }

    #[test]
    fn malformed_flow_id_is_rejected() {
        let mut store = migrated_store();
        let error = run_command(
            &mut store,
            "session.create",
            &serde_json::json!({ "flow_id": "not-a-flow-id" }),
        )
        .expect_err("malformed flow id must fail");

        assert!(error.contains("flow"), "unexpected error: {error}");
        assert!(store.list_sessions().expect("list sessions").is_empty());
    }

    #[test]
    fn nonexistent_flow_is_rejected_before_insert() {
        let mut store = migrated_store();
        let missing = FlowId::new();
        let error = run_command(
            &mut store,
            "session.create",
            &serde_json::json!({ "flow_id": missing }),
        )
        .expect_err("missing flow must fail");

        assert_eq!(error, format!("flow not found: {missing}"));
        assert!(store.list_sessions().expect("list sessions").is_empty());
    }

    #[test]
    fn out_of_range_pid_is_rejected_instead_of_truncated() {
        let mut store = migrated_store();
        let error = run_command(
            &mut store,
            "session.create",
            &serde_json::json!({ "pid": i64::from(i32::MAX) + 1 }),
        )
        .expect_err("out-of-range pid must fail");

        assert!(error.contains("out of range"), "unexpected error: {error}");
        assert!(store.list_sessions().expect("list sessions").is_empty());
    }

    #[test]
    fn valid_existing_flow_is_preserved_on_session() {
        let mut store = migrated_store();
        let flow = agentic_types::Flow::new("Development", "test");
        store.insert_flow(&flow).expect("insert flow");

        let response = run_command(
            &mut store,
            "session.create",
            &serde_json::json!({ "flow_id": flow.id, "pid": 42 }),
        )
        .expect("create session");

        assert_eq!(response["flow_id"], serde_json::json!(flow.id));
        let sessions = store.list_sessions().expect("list sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].flow_id.as_ref(), Some(&flow.id));
    }
}
