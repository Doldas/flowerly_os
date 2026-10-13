//! Per-Session Linux guardian ownership. PTY I/O never awaits the Store lock.
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agentic_core::{coded_error_envelope, ok_envelope};
use agentic_ipc::{Envelope, ErrorCode, Payload};
use agentic_storage::Store;
use agentic_types::{
    ObjectRef, ProvenanceEdge, ProvenanceRelation, Session, SessionId, SessionPresentationState,
    SessionState, Timestamp,
};
use serde_json::{json, Value};

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
}

pub struct Supervisor {
    runtime: PathBuf,
    launch: Mutex<()>,
    sessions: Mutex<HashMap<String, Guardian>>,
}

impl Supervisor {
    pub fn new(runtime: PathBuf) -> Self {
        Self {
            runtime,
            launch: Mutex::new(()),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn routes(&self, name: &str, args: &Value) -> bool {
        if matches!(
            name,
            "session.spawn"
                | "session.attach"
                | "session.detach"
                | "session.read"
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
        } else {
            self.control(store, name, args)
        };
        match result {
            Ok(value) => ok_envelope(request.id.clone(), value),
            Err((code, message)) => coded_error_envelope(request.id.clone(), code, message),
        }
    }

    fn spawn(&self, store: &Arc<Mutex<Store>>, args: &Value) -> Result<Value, Failure> {
        let _serial = self
            .launch
            .lock()
            .map_err(|_| internal("launch lock poisoned"))?;
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
        if args.get("operation_id").is_some_and(|v| !v.is_null()) {
            return Err(invalid(
                "session.spawn cannot safely replay external process effects",
            ));
        }
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
        let socket = self
            .runtime
            .join(format!("pty-{}.sock", session_id.as_str()));
        if socket.as_os_str().as_bytes().len() > 100 {
            return Err(invalid("runtime socket path exceeds Linux limit"));
        }
        let mut guardian = Command::new("python3")
            .args(["-u", "-c", include_str!("../../../scripts/pty-guardian.py")])
            .arg(&socket)
            .arg(shell)
            .arg(std::process::id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| internal(format!("cannot launch PTY guardian: {error}")))?;
        let deadline = Instant::now() + Duration::from_secs(2);
        let pid = loop {
            if let Ok(result) = exchange(&socket, &json!({"op": "status"})) {
                if let Some(pid) = result.get("pid").and_then(Value::as_i64) {
                    break i32::try_from(pid).map_err(|_| internal("child PID overflow"))?;
                }
            }
            if guardian
                .try_wait()
                .map_err(|error| internal(error.to_string()))?
                .is_some()
                || Instant::now() > deadline
            {
                let _ = guardian.kill();
                let _ = guardian.wait();
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
            let _ = exchange(&socket, &json!({"op": "terminate"}));
            return Err((super::storage_error_code(&error), error.to_string()));
        }
        self.sessions
            .lock()
            .map_err(|_| internal("registry lock poisoned"))?
            .insert(
                session_id.to_string(),
                Guardian {
                    process: guardian,
                    socket,
                    actor: actor.to_owned(),
                },
            );
        Ok(
            json!({"id": session_id, "pid": pid, "state": "running", "presentation_state": "detached"}),
        )
    }

    fn mark_unavailable(&self, store: &Arc<Mutex<Store>>, id: &SessionId) -> Result<(), Failure> {
        // SUSPENDED here represents lost supervision, NOT proof of a paused
        // or safely reattachable process; never signal a persisted child PID.
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
        // The core IPC boundary injects kernel-attested created_by on every
        // command. Authorize from the in-memory owner registry so PTY I/O
        // stays responsive while an unrelated SQLite writer is contended.
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
            let exited = guardian
                .process
                .try_wait()
                .map_err(|error| internal(error.to_string()))?
                .is_some();
            (guardian.socket.clone(), exited)
        };
        if supervisor_exited {
            self.mark_unavailable(store, &id)?;
            return Err((
                ErrorCode::Unavailable,
                "PTY guardian exited; no reattachment possible".to_owned(),
            ));
        }
        let op = name.strip_prefix("session.").unwrap_or(name);
        let query = match op {
            "attach" | "read" | "status" => json!({
                "op": op, "cursor": args.get("cursor").cloned().unwrap_or(json!(0))
            }),
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
                // Reconcile only when the actual owned guardian has exited.
                // A transient socket timeout is not proof the child died.
                let exited = self
                    .sessions
                    .lock()
                    .map_err(|_| internal("registry lock poisoned"))?
                    .get_mut(raw)
                    .is_some_and(|entry| entry.process.try_wait().ok().flatten().is_some());
                if exited {
                    self.mark_unavailable(store, &id)?;
                }
                return Err(error);
            }
        };
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
