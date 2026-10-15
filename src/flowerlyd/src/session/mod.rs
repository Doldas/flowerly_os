#![allow(dead_code)]
#[cfg(target_os = "linux")]
mod pty;

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use clap::Parser;
use flowerly_core::ipc::{Envelope, ErrorCode, Payload};
use flowerly_core::storage::repository::{
    CoreCommandRepository, CoreQueryRepository, CreateSession,
};
use flowerly_core::storage::{MutationOperation, StorageError, Store};
use flowerly_core::types::{
    FlowId, ObjectRef, ProvenanceEdge, ProvenanceRelation, Session, SessionId,
    SessionPresentationState, SessionState, Timestamp,
};
use flowerly_core::{
    coded_error_envelope, error_envelope, init_tracing, ok_envelope, run_service_with_status,
    ServiceConfig,
};
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

    #[cfg(target_os = "linux")]
    let cgroup_root = prepare_cgroup_root().map_err(|error| error.to_string());
    #[cfg(target_os = "linux")]
    if let Err(reason) = &cgroup_root {
        tracing::warn!(
            reason,
            "PTY cgroup provisioning unavailable; supervised spawning disabled"
        );
    }
    let store = open_store(&config)?;
    #[cfg(target_os = "linux")]
    for session in store
        .list_sessions()
        .context("listing sessions for PTY recovery")?
    {
        if session.kind == "local-pty" && session.pid.is_some() {
            if let Ok(root) = &cgroup_root {
                terminate_stale_cgroup(&session.id, root.as_deref())?;
            } else {
                tracing::warn!(
                    session_id = %session.id,
                    "PTY cgroup recovery unavailable without delegation"
                );
            }
        }
        if session.kind == "local-pty"
            && session.pid.is_some()
            && session.state == SessionState::Running
        {
            store
                .update_session_state(&session.id, SessionState::Suspended)
                .context("marking former PTY unavailable after restart")?;
            store
                .update_session_presentation_state(&session.id, SessionPresentationState::Detached)
                .context("detaching lost PTY attachment")?;
        }
    }
    let store = Arc::new(Mutex::new(store));
    let status_store = Arc::clone(&store);
    let status = Arc::new(move || match status_store.try_lock() {
        Ok(store) => match CoreQueryRepository::sessions(&*store).map(|items| items.len()) {
            Ok(count) => serde_json::json!({"session_count": count, "ready": true}),
            Err(error) => serde_json::json!({"ready": false, "error": error.to_string()}),
        },
        Err(_) => {
            serde_json::json!({"ready": false, "error": "storage temporarily unavailable or lock poisoned"})
        }
    });
    #[cfg(target_os = "linux")]
    let supervisor = Arc::new(pty::Supervisor::new(
        config.runtime_dir.clone(),
        cgroup_root,
    ));
    #[cfg(target_os = "linux")]
    let lifecycle_reconciler = {
        let supervisor = Arc::clone(&supervisor);
        let store = Arc::clone(&store);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                interval.tick().await;
                let supervisor = Arc::clone(&supervisor);
                let store = Arc::clone(&store);
                let _ = tokio::task::spawn_blocking(move || supervisor.reconcile(&store)).await;
            }
        })
    };
    let handler = move |request: &Envelope| -> Envelope {
        #[cfg(target_os = "linux")]
        if let Payload::Command { name, args } = &request.payload {
            if supervisor.routes(name, args) {
                return supervisor.handle(&store, request);
            }
        }
        handle_command(&store, request)
    };

    let service_result = run_service_with_status(config, status, Arc::new(handler))
        .await
        .context("sessiond terminated abnormally");
    #[cfg(target_os = "linux")]
    lifecycle_reconciler.abort();
    service_result
}

#[cfg(target_os = "linux")]
fn prepare_cgroup_root() -> anyhow::Result<Option<PathBuf>> {
    use std::os::unix::fs::MetadataExt;

    let isolation =
        std::env::var("FLOWERLY_PTY_ISOLATION").unwrap_or_else(|_| "required".to_owned());
    if isolation == "development" {
        return Ok(None);
    }
    if isolation != "required" {
        anyhow::bail!("invalid FLOWERLY_PTY_ISOLATION");
    }
    let root = match std::env::var("FLOWERLY_PTY_CGROUP_ROOT") {
        Ok(value) => PathBuf::from(value),
        Err(_) => {
            let membership = std::fs::read_to_string("/proc/self/cgroup")
                .context("reading Linux process cgroup membership")?;
            let location = membership
                .lines()
                .find_map(|line| line.strip_prefix("0::"))
                .context("unified cgroup v2 process membership not found")?;
            PathBuf::from("/sys/fs/cgroup").join(location.trim_start_matches('/'))
        }
    };
    if !root.is_absolute() {
        anyhow::bail!("delegated cgroup root must be an absolute path");
    }
    let host =
        std::fs::metadata("/sys/fs/cgroup").context("reading unified cgroup v2 mount metadata")?;
    let delegated = std::fs::metadata(&root).context("reading delegated cgroup root metadata")?;
    if host.dev() != delegated.dev() || !root.join("cgroup.controllers").is_file() {
        anyhow::bail!("PTY supervision requires a real delegated Linux cgroup v2 root");
    }
    let management = root.join(format!("management-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir(&management).context("creating PTY supervisor management cgroup")?;
    std::fs::write(
        management.join("cgroup.procs"),
        std::process::id().to_string(),
    )
    .context("moving sessiond into its management cgroup")?;
    std::fs::write(root.join("cgroup.subtree_control"), "+cpu +memory +pids")
        .context("enabling delegated PTY resource controllers")?;
    Ok(Some(root))
}

#[cfg(target_os = "linux")]
fn terminate_stale_cgroup(id: &SessionId, root: Option<&std::path::Path>) -> anyhow::Result<()> {
    let Some(root) = root else {
        return Ok(());
    };
    let Some(identifier) = id.as_str().strip_prefix("session-") else {
        anyhow::bail!("invalid persisted Session cgroup identifier");
    };
    if identifier.len() != 32 || !identifier.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("invalid persisted Session cgroup identifier");
    }
    let location = root.join(format!("pty-{}", id.as_str()));
    match std::fs::symlink_metadata(&location) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => anyhow::bail!("persisted Session cgroup path is not a directory"),
    }
    std::fs::write(location.join("cgroup.kill"), "1")
        .context("killing unrecovered PTY cgroup descendants")?;
    for _ in 0..100 {
        match std::fs::remove_dir(&location) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) if matches!(error.raw_os_error(), Some(16 | 39)) => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!("unrecovered PTY cgroup did not become empty")
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

pub(crate) fn handle_command(store: &Arc<Mutex<Store>>, request: &Envelope) -> Envelope {
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
        Err(error) => coded_error_envelope(request.id.clone(), error.code, error.message),
    }
}

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
                    .map_err(|e| (storage_error_code(&e), e.to_string()))?
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
            let existing = CoreQueryRepository::session(store, &id)
                .map_err(|error| (storage_error_code(&error), error.to_string()))?;
            let session = existing
                .ok_or_else(|| (ErrorCode::NotFound, format!("session not found: {raw}")))?;
            if session.kind == "local-pty" && session.pid.is_some() {
                return Err((
                    ErrorCode::Unavailable,
                    "PTY Session has no currently owned guardian; termination is unavailable"
                        .to_owned(),
                ));
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

fn storage_error_code(error: &StorageError) -> ErrorCode {
    if matches!(error, StorageError::InvalidOperation(_)) {
        ErrorCode::InvalidArgument
    } else if matches!(error, StorageError::OperationConflict(_)) {
        ErrorCode::Conflict
    } else if error.is_busy() {
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

impl std::fmt::Display for CommandFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl PartialEq<String> for CommandFailure {
    fn eq(&self, other: &String) -> bool {
        &self.message == other
    }
}

#[cfg(test)]
impl CommandFailure {
    fn contains(&self, pattern: &str) -> bool {
        self.message.contains(pattern)
    }
}

fn run_command(store: &mut Store, name: &str, args: &Value) -> Result<Value, CommandFailure> {
    match name {
        "session.create" => {
            if args
                .get("operation_id")
                .is_some_and(|value| !value.is_null() && !value.is_string())
            {
                return Err(CommandFailure {
                    code: ErrorCode::InvalidArgument,
                    message: "operation_id must be a string or null".to_owned(),
                });
            }
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
            let intended_response = serde_json::json!({
                "id": session.id,
                "kind": session.kind,
                "state": session.state,
                "presentation_state": session.presentation_state,
                "flow_id": session.flow_id,
            });
            let response = if let Some(operation_id) =
                args.get("operation_id").and_then(Value::as_str)
            {
                let request_json = serde_json::to_string(args).map_err(err_str)?;
                let response_json = serde_json::to_string(&intended_response).map_err(err_str)?;
                let operation = MutationOperation {
                    actor: created_by,
                    command: "session.create",
                    operation_id,
                    request_payload: &request_json,
                    response_payload: &response_json,
                };
                let stored = CoreCommandRepository::create_session_once(
                    store,
                    CreateSession {
                        session: &session,
                        provenance: std::slice::from_ref(&provenance),
                    },
                    &operation,
                )
                .map_err(CommandFailure::storage)?;
                serde_json::from_str(&stored).map_err(err_str)?
            } else {
                CoreCommandRepository::create_session(
                    store,
                    CreateSession {
                        session: &session,
                        provenance: std::slice::from_ref(&provenance),
                    },
                )
                .map_err(CommandFailure::storage)?;
                intended_response
            };
            tracing::info!(session_id = %response["id"], kind, "session create completed");
            Ok(response)
        }
        "session.list" => {
            let sessions = CoreQueryRepository::sessions(store).map_err(CommandFailure::storage)?;
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
            let session = CoreQueryRepository::session(store, &session_id)
                .map_err(CommandFailure::storage)?
                .ok_or_else(|| format!("session not found: {id}"))?;

            store
                .update_session_state(&session_id, SessionState::Exited)
                .map_err(CommandFailure::storage)?;
            store
                .update_session_presentation_state(&session_id, SessionPresentationState::Detached)
                .map_err(CommandFailure::storage)?;
            Ok(serde_json::json!({
                "id": session_id,
                "state": SessionState::Exited,
                "presentation_state": SessionPresentationState::Detached,
                "was": session.state,
                "presentation_was": session.presentation_state,
            }))
        }
        other => Err(format!("sessiond: unknown command `{other}`").into()),
    }
}

fn parse_optional_flow_id(store: &Store, args: &Value) -> Result<Option<FlowId>, CommandFailure> {
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
    if store
        .get_flow(&flow_id)
        .map_err(CommandFailure::storage)?
        .is_none()
    {
        return Err(format!("flow not found: {flow_id}").into());
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

pub(crate) async fn self_test() -> anyhow::Result<()> {
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
    fn typed_session_create_replays_without_duplicate_or_lost_provenance() {
        let mut store = migrated_store();
        let args = serde_json::json!({
            "kind": "metadata-only",
            "created_by": "uid:1000",
            "operation_id": "typed-session-service-1"
        });
        let first = run_command(&mut store, "session.create", &args).expect("first");
        let replay = run_command(&mut store, "session.create", &args).expect("replay");
        assert_eq!(first, replay);
        assert_eq!(store.list_sessions().expect("sessions").len(), 1);
        let id = SessionId::from_str(first["id"].as_str().expect("id")).expect("typed id");
        assert_eq!(
            CoreQueryRepository::provenance_from(&store, &ObjectRef::from(&id))
                .expect("edges")
                .len(),
            1
        );
    }

    #[test]
    fn typed_session_create_rejects_changed_retry_body() {
        let mut store = migrated_store();
        let first = serde_json::json!({
            "kind": "metadata-only",
            "created_by": "uid:1000",
            "operation_id": "typed-session-service-conflict"
        });
        run_command(&mut store, "session.create", &first).expect("create");
        let changed = serde_json::json!({
            "kind": "different-kind",
            "created_by": "uid:1000",
            "operation_id": "typed-session-service-conflict"
        });
        assert!(run_command(&mut store, "session.create", &changed).is_err());
        assert_eq!(store.list_sessions().expect("sessions").len(), 1);
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
        let flow = flowerly_core::types::Flow::new("Development", "test");
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

mod runtime;
pub(crate) use runtime::SessionRuntime;
