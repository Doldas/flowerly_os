#[cfg(target_os = "linux")]
mod pty;

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use flowerly_core::ipc::{Envelope, ErrorCode, Payload};
use flowerly_core::storage::repository::{
    CoreCommandRepository, CoreQueryRepository, CreateSession,
};
use flowerly_core::storage::{MutationOperation, StorageError, Store};
use flowerly_core::types::{Caller, Owner};
use flowerly_core::types::{
    FlowId, ObjectRef, ProvenanceEdge, ProvenanceRelation, Session, SessionId,
    SessionPresentationState, SessionState, Timestamp,
};
use flowerly_core::{coded_error_envelope, error_envelope, ok_envelope};
use serde::Deserialize;
use serde_json::Value;

#[cfg(target_os = "linux")]
pub(crate) struct DelegatedCgroupRoot {
    pub(crate) path: std::path::PathBuf,
    pub(crate) management: String,
}

#[cfg(target_os = "linux")]
fn prepare_cgroup_root() -> anyhow::Result<Option<DelegatedCgroupRoot>> {
    use std::os::unix::fs::MetadataExt;

    let isolation =
        std::env::var("FLOWERLY_PTY_ISOLATION").unwrap_or_else(|_| "required".to_owned());
    if isolation == "development" {
        // Never silent (#240): the shipped unit unsets this variable.
        tracing::warn!("PTY isolation is development: Sessions spawn without cgroup containment");
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
    remove_stale_management_groups(&root);
    let management = format!("management-{}", uuid::Uuid::new_v4().simple());
    std::fs::create_dir(root.join(&management))
        .context("creating PTY supervisor management cgroup")?;
    std::fs::write(
        root.join(&management).join("cgroup.procs"),
        std::process::id().to_string(),
    )
    .context("moving sessiond into its management cgroup")?;
    std::fs::write(root.join("cgroup.subtree_control"), "+cpu +memory +pids")
        .context("enabling delegated PTY resource controllers")?;
    Ok(Some(DelegatedCgroupRoot {
        path: root,
        management,
    }))
}

/// A SIGKILLed instance leaves its `management-<uuid>` group behind, briefly
/// holding its exiting guardians and cleanup watchers. rmdir succeeds only on
/// an empty cgroup, so a live instance's group (it contains that daemon) is
/// never removed; it is retried for at most 2 s.
#[cfg(target_os = "linux")]
fn remove_stale_management_groups(root: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(suffix) = name
            .to_str()
            .and_then(|name| name.strip_prefix("management-"))
        else {
            continue;
        };
        if suffix.len() != 32 || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        for attempt in 0..100 {
            match std::fs::remove_dir(entry.path()) {
                Ok(()) => break,
                Err(error) if error.raw_os_error() == Some(16) && attempt < 99 => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(error) => {
                    tracing::warn!(group = %entry.path().display(), %error,
                        "stale PTY management cgroup not removed");
                    break;
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn terminate_stale_cgroup(
    id: &SessionId,
    root: Option<&DelegatedCgroupRoot>,
) -> anyhow::Result<()> {
    let Some(root) = root else {
        return Ok(());
    };
    let Some(identifier) = id.as_str().strip_prefix("session-") else {
        anyhow::bail!("invalid persisted Session cgroup identifier");
    };
    if identifier.len() != 32 || !identifier.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("invalid persisted Session cgroup identifier");
    }
    let location = root.path.join(format!("pty-{}", id.as_str()));
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

pub(crate) fn handle_command(
    store: &Arc<Mutex<Store>>,
    request: &Envelope,
    caller: Caller,
) -> Envelope {
    let Payload::Command { name, args } = &request.payload else {
        return error_envelope(request.id.clone(), "unexpected request type");
    };
    let mut store = match store.lock() {
        Ok(store) => store,
        Err(_) => return error_envelope(request.id.clone(), "storage lock poisoned"),
    };
    if let Err((code, message)) = validate_session_command(&store, name, args, caller) {
        return coded_error_envelope(request.id.clone(), code, message);
    }
    match run_command(&mut store, name, args, caller) {
        Ok(data) => ok_envelope(request.id.clone(), data),
        Err(error) => coded_error_envelope(request.id.clone(), error.code, error.message),
    }
}

/// Typed IPC-v1 arguments for the metadata Session commands (see
/// `crate::args`). PTY commands keep their own token-checked validation.
#[derive(Debug, Deserialize)]
struct CreateSessionArgs {
    #[serde(default)]
    flow_id: Option<String>,
    #[serde(default)]
    pid: Option<i64>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    operation_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SessionIdArgs {
    id: String,
}

#[derive(Debug, Deserialize)]
struct SessionListArgs {}

fn validate_session_command(
    store: &Store,
    name: &str,
    args: &Value,
    caller: Caller,
) -> Result<(), (ErrorCode, String)> {
    let invalid = |message: &str| (ErrorCode::InvalidArgument, message.to_owned());
    match name {
        "session.create" => {
            let typed: CreateSessionArgs = crate::args::typed_args("session", args)?;
            // Type-checked here; the handler applies them.
            let _ = (&typed.kind, &typed.cwd, &typed.operation_id);
            if let Some(raw) = typed.flow_id.as_deref() {
                let flow_id = FlowId::from_str(raw).map_err(|e| invalid(&e.to_string()))?;
                // Own Flow or General only; rechecked when inserting (#225).
                if CoreQueryRepository::flow(store, caller, &flow_id)
                    .map_err(|e| (storage_error_code(&e), e.to_string()))?
                    .is_none()
                {
                    return Err((ErrorCode::NotFound, format!("flow not found: {flow_id}")));
                }
            }
            if let Some(raw) = typed.pid {
                i32::try_from(raw).map_err(|_| invalid("pid out of range for i32"))?;
            }
        }
        "session.list" => {
            let _: SessionListArgs = crate::args::typed_args("session", args)?;
        }
        "session.terminate" => {
            let typed: SessionIdArgs = crate::args::typed_args("session", args)?;
            let raw = typed.id.as_str();
            let id = SessionId::from_str(raw).map_err(|e| invalid(&e.to_string()))?;
            let existing = CoreQueryRepository::session(store, caller, &id)
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
    error.ipc_error_code()
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

fn run_command(
    store: &mut Store,
    name: &str,
    args: &Value,
    caller: Caller,
) -> Result<Value, CommandFailure> {
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
            let flow_id = parse_optional_flow_id(store, args, caller)?;
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
                owner: caller.owner(),
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
            let sessions =
                CoreQueryRepository::sessions(store, caller).map_err(CommandFailure::storage)?;
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
            let session = CoreCommandRepository::terminate_session(store, caller, &session_id)
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

/// A Flow the caller may attach a Session to: its own or General.
fn parse_optional_flow_id(
    store: &Store,
    args: &Value,
    caller: Caller,
) -> Result<Option<FlowId>, CommandFailure> {
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
    if CoreQueryRepository::flow(store, caller, &flow_id)
        .map_err(CommandFailure::storage)?
        .is_none()
    {
        return Err(CommandFailure {
            code: ErrorCode::NotFound,
            message: format!("flow not found: {flow_id}"),
        });
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
        owner: Owner::System,
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

    const TEST_CALLER: Caller = Caller::new(1000);

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
        let first = run_command(&mut store, "session.create", &args, TEST_CALLER).expect("first");
        let replay = run_command(&mut store, "session.create", &args, TEST_CALLER).expect("replay");
        assert_eq!(first, replay);
        assert_eq!(store.list_sessions().expect("sessions").len(), 1);
        let id = SessionId::from_str(first["id"].as_str().expect("id")).expect("typed id");
        assert_eq!(
            store
                .provenance_from(&ObjectRef::from(&id))
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
        run_command(&mut store, "session.create", &first, TEST_CALLER).expect("create");
        let changed = serde_json::json!({
            "kind": "different-kind",
            "created_by": "uid:1000",
            "operation_id": "typed-session-service-conflict"
        });
        assert!(run_command(&mut store, "session.create", &changed, TEST_CALLER).is_err());
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
                validate_session_command(&store, command, &args, TEST_CALLER)
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
            TEST_CALLER,
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
            TEST_CALLER,
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
            TEST_CALLER,
        )
        .expect_err("out-of-range pid must fail");

        assert!(error.contains("out of range"), "unexpected error: {error}");
        assert!(store.list_sessions().expect("list sessions").is_empty());
    }

    #[test]
    fn sessions_attach_only_to_own_flows_or_general_and_stay_private() {
        let mut store = migrated_store();
        let general = store.ensure_general_flow().unwrap();
        let other = Caller::new(2000);
        let mut theirs = flowerly_core::types::Flow::new("Theirs", "uid:2000");
        theirs.owner = other.owner();
        store.insert_flow(&theirs).expect("insert flow");
        let args = serde_json::json!({ "flow_id": theirs.id, "kind": "metadata" });
        let (code, _) =
            validate_session_command(&store, "session.create", &args, TEST_CALLER).unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
        let failure = run_command(&mut store, "session.create", &args, TEST_CALLER).unwrap_err();
        assert_eq!(failure.code, ErrorCode::NotFound);
        assert!(store.list_sessions().unwrap().is_empty());

        let in_general = serde_json::json!({ "flow_id": general, "kind": "metadata" });
        let created = run_command(&mut store, "session.create", &in_general, other).unwrap();
        let id = serde_json::json!({ "id": created["id"] });
        let (code, _) =
            validate_session_command(&store, "session.terminate", &id, TEST_CALLER).unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
        assert_eq!(
            run_command(&mut store, "session.terminate", &id, TEST_CALLER)
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        let listed = run_command(
            &mut store,
            "session.list",
            &serde_json::json!({}),
            TEST_CALLER,
        )
        .unwrap();
        assert_eq!(listed["count"], 0);
        run_command(&mut store, "session.terminate", &id, other).expect("owner may terminate");
    }

    #[test]
    fn valid_existing_flow_is_preserved_on_session() {
        let mut store = migrated_store();
        let mut flow = flowerly_core::types::Flow::new("Development", "test");
        flow.owner = TEST_CALLER.owner();
        store.insert_flow(&flow).expect("insert flow");

        let response = run_command(
            &mut store,
            "session.create",
            &serde_json::json!({ "flow_id": flow.id, "pid": 42 }),
            TEST_CALLER,
        )
        .expect("create session");

        assert_eq!(response["flow_id"], serde_json::json!(flow.id));
        let sessions = store.list_sessions().expect("list sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].flow_id.as_ref(), Some(&flow.id));
    }

    fn ipc(store: &Arc<Mutex<Store>>, command: &str, args: Value) -> Result<Value, ErrorCode> {
        let request = Envelope::new(Payload::Command {
            name: command.into(),
            args,
        });
        match handle_command(store, &request, TEST_CALLER).payload {
            Payload::Ok { data } => Ok(data),
            Payload::Err { code, .. } => Err(code.expect("typed error code")),
            other => panic!("unexpected reply {other:?}"),
        }
    }

    #[test]
    fn typed_v1_arguments_contract() {
        let store = Arc::new(Mutex::new(migrated_store()));
        let created = ipc(
            &store,
            "session.create",
            serde_json::json!({"kind": "metadata", "cwd": "/tmp", "flow_id": null,
                               "pid": null, "operation_id": null, "future": [1]}),
        )
        .expect("create with nulls and unknown field");
        ipc(&store, "session.list", serde_json::json!({"page": 1})).expect("list");
        for (command, args) in [
            // Previously `kind` silently became "local-pty" and `cwd` was dropped.
            ("session.create", serde_json::json!({"kind": 42})),
            ("session.create", serde_json::json!({"cwd": ["/tmp"]})),
            ("session.create", serde_json::json!({"pid": "1"})),
            (
                "session.create",
                serde_json::json!({"pid": 9_999_999_999_i64}),
            ),
            ("session.create", serde_json::json!({"flow_id": 3})),
            ("session.create", serde_json::json!({"operation_id": 5})),
            ("session.terminate", serde_json::json!({"id": 7})),
            ("session.terminate", serde_json::json!({})),
            ("session.list", serde_json::json!("all")),
        ] {
            assert_eq!(
                ipc(&store, command, args.clone()),
                Err(ErrorCode::InvalidArgument),
                "{command} {args}"
            );
        }
        let id = created["id"].as_str().expect("session id").to_owned();
        assert!(ipc(
            &store,
            "session.terminate",
            serde_json::json!({"id": id, "extra": 1})
        )
        .is_ok());
    }
}

mod runtime;
pub(crate) use runtime::SessionRuntime;
