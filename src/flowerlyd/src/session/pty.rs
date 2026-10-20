use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flowerly_core::ipc::{Envelope, ErrorCode, Payload};
use flowerly_core::storage::Store;
use flowerly_core::types::{
    ObjectRef, ProvenanceEdge, ProvenanceRelation, Session, SessionId, SessionPresentationState,
    SessionState, Timestamp,
};
use flowerly_core::{coded_error_envelope, ok_envelope};
use serde_json::{json, Value};
use uuid::Uuid;

#[cfg(target_os = "linux")]
use super::DelegatedCgroupRoot;

type Failure = (ErrorCode, String);

fn invalid(message: impl Into<String>) -> Failure {
    (ErrorCode::InvalidArgument, message.into())
}

fn internal(message: impl Into<String>) -> Failure {
    (ErrorCode::Internal, message.into())
}

struct Guardian {
    process: Child,
    socket: PathBuf,
    actor: String,
    control_token: String,
    writer: Option<WriterLease>,
    command_lock: Arc<Mutex<()>>,
    finished_at: Option<Instant>,
}

struct WriterLease {
    token: String,
    expires_at: Instant,
}

struct PendingGuardian {
    process: Option<Child>,
    socket: PathBuf,
}

impl PendingGuardian {
    fn new(process: Child, socket: PathBuf) -> Self {
        Self {
            process: Some(process),
            socket,
        }
    }

    fn process(&mut self) -> &mut Child {
        self.process.as_mut().expect("pending guardian process")
    }

    fn into_owned(mut self) -> Child {
        self.process.take().expect("pending guardian ownership")
    }
}

impl Drop for PendingGuardian {
    fn drop(&mut self) {
        let Some(mut process) = self.process.take() else {
            return;
        };
        let _ = exchange(&self.socket, &json!({"op": "terminate"}));
        for _ in 0..10 {
            if process.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = process.kill();
        let _ = process.wait();
    }
}

const WRITER_LEASE: Duration = Duration::from_secs(60);

pub struct Supervisor {
    runtime: PathBuf,
    cgroup_root: Result<Option<DelegatedCgroupRoot>, String>,
    launch: Mutex<()>,
    sessions: Mutex<HashMap<String, Guardian>>,
}

impl Supervisor {
    pub fn new(runtime: PathBuf, cgroup_root: Result<Option<DelegatedCgroupRoot>, String>) -> Self {
        Self {
            runtime,
            cgroup_root,
            launch: Mutex::new(()),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn routes(&self, name: &str, args: &Value) -> bool {
        if matches!(
            name,
            "session.spawn"
                | "session.spawn.lookup"
                | "session.attach"
                | "session.detach"
                | "session.heartbeat"
                | "session.read"
                | "session.follow"
                | "session.write"
                | "session.resize"
                | "session.status"
        ) {
            return true;
        }
        name == "session.terminate"
            && args.get("id").and_then(Value::as_str).is_some_and(|id| {
                self.sessions
                    .lock()
                    .is_ok_and(|items| items.contains_key(id))
            })
    }

    pub fn handle(&self, store: &Arc<Mutex<Store>>, request: &Envelope) -> Envelope {
        let Payload::Command { name, args } = &request.payload else {
            return coded_error_envelope(
                request.id.clone(),
                ErrorCode::InvalidArgument,
                "command required",
            );
        };
        let result = if name == "session.spawn" {
            self.spawn(store, args)
        } else if name == "session.spawn.lookup" {
            self.lookup_spawn(store, args)
        } else {
            self.control(store, name, args)
        };
        match result {
            Ok(value) => ok_envelope(request.id.clone(), value),
            Err((code, message)) => coded_error_envelope(request.id.clone(), code, message),
        }
    }

    pub fn reconcile(&self, store: &Arc<Mutex<Store>>) -> Result<(), Failure> {
        let candidates = {
            let sessions = self
                .sessions
                .lock()
                .map_err(|_| internal("registry lock poisoned"))?;
            sessions
                .iter()
                .map(|(id, entry)| (id.clone(), Arc::clone(&entry.command_lock)))
                .collect::<Vec<_>>()
        };
        for (raw, command_lock) in candidates {
            let Ok(_serial) = command_lock.try_lock() else {
                continue;
            };
            let observed = {
                let mut sessions = self
                    .sessions
                    .lock()
                    .map_err(|_| internal("registry lock poisoned"))?;
                let Some(entry) = sessions.get_mut(&raw) else {
                    continue;
                };
                let exited = entry
                    .process
                    .try_wait()
                    .map_err(|error| internal(error.to_string()))?
                    .is_some();
                (entry.socket.clone(), exited, entry.finished_at)
            };
            let id = SessionId::from_str(&raw).map_err(|error| internal(error.to_string()))?;
            if observed.1 {
                self.mark_unavailable(store, &id)?;
                self.sessions
                    .lock()
                    .map_err(|_| internal("registry lock poisoned"))?
                    .remove(&raw);
                continue;
            }
            if observed
                .2
                .is_some_and(|completed| completed.elapsed() >= Duration::from_secs(20))
            {
                let _ = exchange(&observed.0, &json!({"op": "terminate"}));
                if let Some(mut entry) = self
                    .sessions
                    .lock()
                    .map_err(|_| internal("registry lock poisoned"))?
                    .remove(&raw)
                {
                    let _ = entry.process.wait();
                }
                continue;
            }
            if observed.2.is_some() {
                continue;
            }
            let Ok(status) = exchange(&observed.0, &json!({"op": "lifecycle"})) else {
                continue;
            };
            if status.get("running") != Some(&Value::Bool(false)) {
                continue;
            }
            {
                let db = store.lock().map_err(|_| internal("store lock poisoned"))?;
                db.update_session_state(&id, SessionState::Exited)
                    .map_err(|error| (super::storage_error_code(&error), error.to_string()))?;
                db.update_session_presentation_state(&id, SessionPresentationState::Detached)
                    .map_err(|error| (super::storage_error_code(&error), error.to_string()))?;
            }
            if let Some(entry) = self
                .sessions
                .lock()
                .map_err(|_| internal("registry lock poisoned"))?
                .get_mut(&raw)
            {
                entry.finished_at = Some(Instant::now());
            }
        }
        Ok(())
    }

    fn spawn(&self, store: &Arc<Mutex<Store>>, args: &Value) -> Result<Value, Failure> {
        let _serial = self
            .launch
            .lock()
            .map_err(|_| internal("launch lock poisoned"))?;
        if let Err(reason) = &self.cgroup_root {
            return Err((
                ErrorCode::Unavailable,
                format!("PTY cgroup isolation unavailable: {reason}"),
            ));
        }
        let stale = {
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|_| internal("registry lock poisoned"))?;
            let mut ids = Vec::new();
            for (id, guardian) in sessions.iter_mut() {
                if guardian
                    .process
                    .try_wait()
                    .map_err(|error| internal(error.to_string()))?
                    .is_some()
                {
                    ids.push(id.clone());
                }
            }
            for id in &ids {
                sessions.remove(id);
            }
            ids
        };
        for raw in stale {
            let id = SessionId::from_str(&raw).map_err(|error| internal(error.to_string()))?;
            self.mark_unavailable(store, &id)?;
        }
        if self
            .sessions
            .lock()
            .map_err(|_| internal("registry lock poisoned"))?
            .len()
            >= 16
        {
            return Err((
                ErrorCode::Unavailable,
                "PTY session limit (16) reached".to_owned(),
            ));
        }
        // An external effect cannot be rolled back by SQLite. Persist the key
        // before Command::spawn. On retry, fail closed even after daemon restart;
        // the outcome may still be unknown and must NOT launch another shell.
        let operation_id = match args.get("operation_id") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) => Some(value.as_str()),
            Some(_) => return Err(invalid("operation_id must be a string or null")),
        };
        if args.get("pid").is_some() {
            return Err(invalid("session.spawn does not accept a caller PID"));
        }
        let shell = args
            .get("shell")
            .and_then(Value::as_str)
            .unwrap_or("/bin/sh");
        if !matches!(shell, "/bin/sh" | "/bin/bash") {
            return Err(invalid("shell not allowlisted"));
        }
        let actor = args
            .get("created_by")
            .and_then(Value::as_str)
            .unwrap_or("user");
        let flow_id = {
            let db = store.lock().map_err(|_| internal("store lock poisoned"))?;
            super::parse_optional_flow_id(&db, args).map_err(|error| (error.code, error.message))?
        };
        let session_id = SessionId::new();
        if let Some(operation_id) = operation_id {
            let payload = json!({
                "shell": shell,
                "created_by": actor,
                "flow_id": flow_id.as_ref().map(ToString::to_string)
            })
            .to_string();
            let reserved = store
                .lock()
                .map_err(|_| internal("store lock poisoned"))?
                .reserve_external_spawn(actor, operation_id, &payload, &session_id)
                .map_err(|error| (super::storage_error_code(&error), error.to_string()))?;
            if !reserved {
                return Err((
                    ErrorCode::Unavailable,
                    "session.spawn prior attempt has an unknown external outcome; no automatic retry".to_owned(),
                ));
            }
        }
        let socket = self
            .runtime
            .join(format!("pty-{}.sock", session_id.as_str()));
        if socket.as_os_str().as_bytes().len() > 100 {
            return Err(invalid("runtime socket path exceeds Linux limit"));
        }
        let mut command = Command::new("python3");
        command
            .args([
                "-u",
                "-c",
                include_str!("../../../../scripts/pty-guardian.py"),
            ])
            .arg(&socket)
            .arg(shell)
            .arg(std::process::id().to_string())
            .env(
                "FLOWERLY_PTY_CGROUP_NAME",
                format!("pty-{}", session_id.as_str()),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match &self.cgroup_root {
            Ok(Some(root)) => {
                command
                    .env("FLOWERLY_PTY_ISOLATION", "required")
                    .env("FLOWERLY_PTY_CGROUP_ROOT", &root.path)
                    .env("FLOWERLY_PTY_CGROUP_MANAGEMENT", &root.management);
            }
            Ok(None) => {
                command
                    .env("FLOWERLY_PTY_ISOLATION", "development")
                    .env_remove("FLOWERLY_PTY_CGROUP_ROOT");
            }
            Err(reason) => {
                return Err((
                    ErrorCode::Unavailable,
                    format!("PTY cgroup isolation unavailable: {reason}"),
                ));
            }
        }
        let mut pending = PendingGuardian::new(
            command
                .spawn()
                .map_err(|error| internal(format!("cannot launch PTY guardian: {error}")))?,
            socket.clone(),
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        let pid = loop {
            if let Ok(result) = exchange(&socket, &json!({"op": "status"})) {
                if let Some(pid) = result.get("pid").and_then(Value::as_i64) {
                    break i32::try_from(pid).map_err(|_| internal("child PID overflow"))?;
                }
            }
            if pending
                .process()
                .try_wait()
                .map_err(|error| internal(error.to_string()))?
                .is_some()
                || Instant::now() > deadline
            {
                return Err((ErrorCode::Unavailable, "PTY startup failed".to_owned()));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let session = Session {
            id: session_id.clone(),
            flow_id,
            kind: "local-pty".to_owned(),
            pid: Some(pid),
            cwd: None,
            state: SessionState::Running,
            presentation_state: SessionPresentationState::Detached,
            created_by: actor.to_owned(),
            created_at: Timestamp::now(),
        };
        let provenance = ProvenanceEdge {
            from: ObjectRef::from(&session_id),
            relation: ProvenanceRelation::CreatedBy,
            to: ObjectRef::actor(actor).map_err(|error| invalid(error.to_string()))?,
            created_at: Timestamp::now(),
            note: None,
        };
        let insertion = store
            .lock()
            .map_err(|_| internal("store lock poisoned"))?
            .insert_session_with_provenance(&session, &[provenance]);
        if let Err(error) = insertion {
            return Err((super::storage_error_code(&error), error.to_string()));
        }
        let supervisor_pid = pending.process().id();
        let control_token = Uuid::new_v4().to_string();
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| internal("registry lock poisoned"))?;
        sessions.insert(
            session_id.to_string(),
            Guardian {
                process: pending.into_owned(),
                socket,
                actor: actor.to_owned(),
                control_token: control_token.clone(),
                writer: None,
                command_lock: Arc::new(Mutex::new(())),
                finished_at: None,
            },
        );
        Ok(
            json!({"id": session_id, "pid": pid, "supervisor_pid": supervisor_pid, "control_token": control_token, "state": "running", "presentation_state": "detached"}),
        )
    }

    /// A lookup never adopts a PID, grants a control token or starts a shell.
    /// It only identifies the reserved Session and whether its guardian is
    /// currently supervised by this daemon instance.
    fn lookup_spawn(&self, store: &Arc<Mutex<Store>>, args: &Value) -> Result<Value, Failure> {
        let key = args
            .get("operation_id")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("operation_id must be a string"))?;
        let actor = args
            .get("created_by")
            .and_then(Value::as_str)
            .unwrap_or("user");
        let id = store
            .lock()
            .map_err(|_| internal("store lock poisoned"))?
            .lookup_external_spawn(actor, key)
            .map_err(|error| (super::storage_error_code(&error), error.to_string()))?
            .ok_or_else(|| (ErrorCode::NotFound, "unknown operation_id".to_owned()))?;
        let supervised = self
            .sessions
            .lock()
            .map_err(|_| internal("registry lock poisoned"))?
            .get(id.as_str())
            .is_some_and(|guardian| guardian.actor == actor);
        let durable = store
            .lock()
            .map_err(|_| internal("store lock poisoned"))?
            .get_session(&id)
            .map_err(|error| (super::storage_error_code(&error), error.to_string()))?
            .is_some();
        Ok(json!({
            "session_id": id,
            "reservation": "durable",
            "session_recorded": durable,
            "guardian_supervised": supervised,
            "external_outcome": if supervised { "supervised" } else { "unknown" },
            "retry_safe": false
        }))
    }

    fn mark_unavailable(&self, store: &Arc<Mutex<Store>>, id: &SessionId) -> Result<(), Failure> {
        let db = store.lock().map_err(|_| internal("store lock poisoned"))?;
        db.update_session_state(id, SessionState::Suspended)
            .map_err(|error| (super::storage_error_code(&error), error.to_string()))?;
        db.update_session_presentation_state(id, SessionPresentationState::Detached)
            .map_err(|error| (super::storage_error_code(&error), error.to_string()))?;
        Ok(())
    }

    fn control(
        &self,
        store: &Arc<Mutex<Store>>,
        name: &str,
        args: &Value,
    ) -> Result<Value, Failure> {
        let raw = args
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("missing string session id"))?;
        let id = SessionId::from_str(raw).map_err(|error| invalid(error.to_string()))?;
        let actor = args
            .get("created_by")
            .and_then(Value::as_str)
            .unwrap_or("user");
        let command_lock = {
            let sessions = self
                .sessions
                .lock()
                .map_err(|_| internal("registry lock poisoned"))?;
            Arc::clone(
                &sessions
                    .get(raw)
                    .ok_or_else(|| {
                        (
                            ErrorCode::Unavailable,
                            "PTY is no longer supervised".to_owned(),
                        )
                    })?
                    .command_lock,
            )
        };
        let _command_guard = command_lock
            .lock()
            .map_err(|_| internal("PTY command lock poisoned"))?;
        let mut issued_token = None;
        let (socket, supervisor_exited) = {
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|_| internal("registry lock poisoned"))?;
            let guardian = sessions.get_mut(raw).ok_or_else(|| {
                (
                    ErrorCode::Unavailable,
                    "PTY guardian unavailable (restart or exit)".to_owned(),
                )
            })?;
            if guardian.actor != actor {
                return Err((
                    ErrorCode::Unavailable,
                    "session actor not authorized".to_owned(),
                ));
            }
            if matches!(
                name,
                "session.attach" | "session.read" | "session.follow" | "session.terminate"
            ) {
                let supplied = args.get("control_token").and_then(Value::as_str);
                if supplied != Some(guardian.control_token.as_str()) {
                    return Err((
                        ErrorCode::Conflict,
                        "session control token required or invalid".to_owned(),
                    ));
                }
            }
            let exited = guardian
                .process
                .try_wait()
                .map_err(|error| internal(error.to_string()))?
                .is_some();
            if !exited {
                if guardian
                    .writer
                    .as_ref()
                    .is_some_and(|lease| lease.expires_at <= Instant::now())
                {
                    guardian.writer = None;
                }
                match name {
                    "session.attach" => {
                        if guardian.writer.is_some() {
                            return Err((
                                ErrorCode::Conflict,
                                "Session already has an active writer".to_owned(),
                            ));
                        }
                        let token = Uuid::new_v4().to_string();
                        guardian.writer = Some(WriterLease {
                            token: token.clone(),
                            expires_at: Instant::now() + WRITER_LEASE,
                        });
                        issued_token = Some(token);
                    }
                    "session.write" | "session.resize" | "session.detach" | "session.heartbeat" => {
                        let requested = args
                            .get("attachment_token")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                invalid("attachment_token required for terminal writer operations")
                            })?;
                        let lease = guardian.writer.as_mut().ok_or_else(|| {
                            (
                                ErrorCode::Conflict,
                                "no active writer attachment".to_owned(),
                            )
                        })?;
                        if requested != lease.token {
                            return Err((
                                ErrorCode::Conflict,
                                "writer attachment token revoked or incorrect".to_owned(),
                            ));
                        }
                        lease.expires_at = Instant::now() + WRITER_LEASE;
                    }
                    _ => {}
                }
            }
            (guardian.socket.clone(), exited)
        };
        if supervisor_exited {
            self.mark_unavailable(store, &id)?;
            return Err((
                ErrorCode::Unavailable,
                "PTY guardian exited; no reattachment possible".to_owned(),
            ));
        }
        if name == "session.heartbeat" {
            return Ok(json!({"lease_active": true, "expires_in_ms": 60000}));
        }
        let op = name.strip_prefix("session.").unwrap_or(name);
        let query = match op {
            "attach" | "read" | "status" => json!({
                "op": op, "cursor": args.get("cursor").cloned().unwrap_or(json!(0))
            }),
            "follow" => {
                let wait_ms = args.get("wait_ms").cloned().unwrap_or(json!(100));
                let value = wait_ms
                    .as_u64()
                    .filter(|value| *value <= 250)
                    .ok_or_else(|| {
                        invalid("session.follow wait_ms must be an integer from 0 to 250")
                    })?;
                json!({"op": "follow", "cursor": args.get("cursor").cloned().unwrap_or(json!(0)), "wait_ms": value})
            }
            "write" => {
                let text = args
                    .get("data")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("session.write requires text data"))?;
                if text.len() > 4096 {
                    return Err(invalid("session.write exceeds 4096 bytes"));
                }
                let encoded: String = text
                    .as_bytes()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                json!({"op": "write", "data_hex": encoded})
            }
            "resize" => json!({
                "op": "resize", "rows": args.get("rows"), "cols": args.get("cols")
            }),
            "detach" => json!({"op": "status"}),
            "terminate" => json!({"op": "terminate"}),
            _ => return Err(invalid("unknown PTY command")),
        };
        let response = match exchange(&socket, &query) {
            Ok(response) => response,
            Err(error) => {
                let exited = self
                    .sessions
                    .lock()
                    .map_err(|_| internal("registry lock poisoned"))?
                    .get_mut(raw)
                    .is_some_and(|entry| entry.process.try_wait().ok().flatten().is_some());
                if let Some(ref token) = issued_token {
                    if let Ok(mut entries) = self.sessions.lock() {
                        if let Some(entry) = entries.get_mut(raw) {
                            if entry
                                .writer
                                .as_ref()
                                .is_some_and(|lease| &lease.token == token)
                            {
                                entry.writer = None;
                            }
                        }
                    }
                }
                if exited {
                    self.mark_unavailable(store, &id)?;
                }
                return Err(error);
            }
        };
        let mut response = response;
        if op == "status"
            && args.get("control_token").and_then(Value::as_str)
                != self
                    .sessions
                    .lock()
                    .map_err(|_| internal("registry lock poisoned"))?
                    .get(raw)
                    .map(|entry| entry.control_token.as_str())
        {
            if let Some(data) = response.as_object_mut() {
                data.remove("output_base64");
            }
        }
        if op == "terminate" || response.get("running") == Some(&Value::Bool(false)) {
            store
                .lock()
                .map_err(|_| internal("store lock poisoned"))?
                .update_session_state(&id, SessionState::Exited)
                .map_err(|error| (super::storage_error_code(&error), error.to_string()))?;
        }
        if op == "attach" || op == "detach" {
            let presentation = if op == "attach" {
                SessionPresentationState::Attached
            } else {
                SessionPresentationState::Detached
            };
            store
                .lock()
                .map_err(|_| internal("store lock poisoned"))?
                .update_session_presentation_state(&id, presentation)
                .map_err(|error| (super::storage_error_code(&error), error.to_string()))?;
        }
        if op == "terminate" {
            if let Some(mut guardian) = self
                .sessions
                .lock()
                .map_err(|_| internal("registry lock poisoned"))?
                .remove(raw)
            {
                let _ = guardian.process.wait();
            }
        }
        if op == "detach" {
            if let Ok(mut entries) = self.sessions.lock() {
                if let Some(entry) = entries.get_mut(raw) {
                    entry.writer = None;
                }
            }
        }
        if let Some(token) = issued_token {
            let mut result = response;
            result["attachment_token"] = json!(token);
            return Ok(result);
        }
        Ok(response)
    }
}

fn exchange(socket: &Path, request: &Value) -> Result<Value, Failure> {
    let mut stream =
        UnixStream::connect(socket).map_err(|error| (ErrorCode::Unavailable, error.to_string()))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| internal(error.to_string()))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| internal(error.to_string()))?;
    writeln!(stream, "{request}").map_err(|error| internal(error.to_string()))?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|error| (ErrorCode::Unavailable, error.to_string()))?;
    let response: Value =
        serde_json::from_str(&line).map_err(|error| internal(error.to_string()))?;
    if let Some(reason) = response.get("error").and_then(Value::as_str) {
        return Err((ErrorCode::Unavailable, reason.to_owned()));
    }
    Ok(response)
}
