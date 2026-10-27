use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flowerly_core::ipc::{Envelope, ErrorCode, Payload};
use flowerly_core::storage::Store;
use flowerly_core::types::Caller;
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
    waiting_followers: Arc<AtomicUsize>,
    finished_at: Option<Instant>,
}

struct WriterLease {
    token: String,
    expires_at: Instant,
}

impl WriterLease {
    fn issue(now: Instant) -> Self {
        Self {
            token: Uuid::new_v4().to_string(),
            expires_at: now + WRITER_LEASE,
        }
    }
}

/// Drop a writer lease whose deadline has passed, so a new attach can win.
fn expire_writer(writer: &mut Option<WriterLease>, now: Instant) {
    if writer.as_ref().is_some_and(|lease| lease.expires_at <= now) {
        *writer = None;
    }
}

/// Exclusive writer attachment: refused while another live lease exists.
fn attach_writer(writer: &mut Option<WriterLease>, now: Instant) -> Result<String, Failure> {
    expire_writer(writer, now);
    if writer.is_some() {
        return Err((
            ErrorCode::Conflict,
            "Session already has an active writer".to_owned(),
        ));
    }
    let lease = WriterLease::issue(now);
    let token = lease.token.clone();
    *writer = Some(lease);
    Ok(token)
}

/// Authorize a writer operation and extend the lease on success. An expired,
/// revoked or foreign token never authorizes and never extends a lease.
fn renew_writer(
    writer: &mut Option<WriterLease>,
    requested: &str,
    now: Instant,
) -> Result<(), Failure> {
    expire_writer(writer, now);
    let lease = writer.as_mut().ok_or_else(|| {
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
    lease.expires_at = now + WRITER_LEASE;
    Ok(())
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
const MAX_WAITING_FOLLOWERS: usize = 16;

struct FollowPermit(Arc<AtomicUsize>);

impl Drop for FollowPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

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

    /// Kernel-owned runtime directory is tied to the same UID as the
    /// credential-checked Unix socket. Do not use client JSON `created_by`
    /// as an authorization principal for durable external operations.
    fn principal(&self) -> Result<String, Failure> {
        let owner = std::fs::metadata(&self.runtime)
            .map_err(|error| internal(format!("runtime ownership unavailable: {error}")))?
            .uid();
        Ok(format!("uid:{owner}"))
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

    pub fn handle(
        &self,
        store: &Arc<Mutex<Store>>,
        request: &Envelope,
        caller: Caller,
    ) -> Envelope {
        let Payload::Command { name, args } = &request.payload else {
            return coded_error_envelope(
                request.id.clone(),
                ErrorCode::InvalidArgument,
                "command required",
            );
        };
        let result = if name == "session.spawn" {
            self.spawn(store, args, caller)
        } else if name == "session.spawn.lookup" {
            self.lookup_spawn(store, args, caller)
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

    fn spawn(
        &self,
        store: &Arc<Mutex<Store>>,
        args: &Value,
        caller: Caller,
    ) -> Result<Value, Failure> {
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
            super::parse_optional_flow_id(&db, args, caller)
                .map_err(|error| (error.code, error.message))?
        };
        let session_id = SessionId::new();
        if operation_id.is_none() {
            // Every launch is durably recorded before Command::spawn (#74):
            // restart recovery must find this cgroup even if the Session row
            // never commits.
            store
                .lock()
                .map_err(|_| internal("store lock poisoned"))?
                .reserve_unkeyed_spawn(&self.principal()?, &session_id)
                .map_err(|error| (super::storage_error_code(&error), error.to_string()))?;
        }
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
                .reserve_external_spawn(&self.principal()?, operation_id, &payload, &session_id)
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
            // PTY commands are service-account-only (#222), so this is the
            // kernel-attested service account, never a request field.
            owner: caller.owner(),
        };
        let provenance = ProvenanceEdge {
            from: ObjectRef::from(&session_id),
            relation: ProvenanceRelation::CreatedBy,
            to: ObjectRef::actor(actor).map_err(|error| invalid(error.to_string()))?,
            created_at: Timestamp::now(),
            note: None,
        };
        let insertion = {
            let mut db = store.lock().map_err(|_| internal("store lock poisoned"))?;
            db.insert_session_with_provenance(&session, &[provenance])
                .and_then(|()| {
                    if operation_id.is_none() {
                        db.release_unkeyed_spawn(&session_id)
                    } else {
                        Ok(())
                    }
                })
        };
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
                waiting_followers: Arc::new(AtomicUsize::new(0)),
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
    fn lookup_spawn(
        &self,
        store: &Arc<Mutex<Store>>,
        args: &Value,
        caller: Caller,
    ) -> Result<Value, Failure> {
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
            .lookup_external_spawn(&self.principal()?, key)
            .map_err(|error| (super::storage_error_code(&error), error.to_string()))?
            .ok_or_else(|| (ErrorCode::NotFound, "unknown operation_id".to_owned()))?;
        let supervised = self
            .sessions
            .lock()
            .map_err(|_| internal("registry lock poisoned"))?
            .get_mut(id.as_str())
            .is_some_and(|guardian| {
                guardian.actor == actor
                    && guardian
                        .process
                        .try_wait()
                        .is_ok_and(|result| result.is_none())
            });
        let durable = store
            .lock()
            .map_err(|_| internal("store lock poisoned"))?
            .session_for(caller, &id)
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
        // Long-poll observers must never hold the Session command lock while
        // waiting for output. Yield between short nonblocking snapshots.
        if name == "session.follow" {
            let wait_ms = match args.get("wait_ms") {
                None => 100,
                Some(value) => value
                    .as_u64()
                    .filter(|value| *value <= 250)
                    .ok_or_else(|| {
                        invalid("session.follow wait_ms must be an integer from 0 to 250")
                    })?,
            };
            if wait_ms > 0 {
                let counter = {
                    let sessions = self
                        .sessions
                        .lock()
                        .map_err(|_| internal("registry lock poisoned"))?;
                    let id = args
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| invalid("missing string session id"))?;
                    let guardian = sessions.get(id).ok_or_else(|| {
                        (
                            ErrorCode::Unavailable,
                            "PTY is no longer supervised".to_owned(),
                        )
                    })?;
                    if args.get("control_token").and_then(Value::as_str)
                        != Some(guardian.control_token.as_str())
                    {
                        return Err((
                            ErrorCode::Conflict,
                            "session control token required or invalid".to_owned(),
                        ));
                    }
                    Arc::clone(&guardian.waiting_followers)
                };
                let mut count = counter.load(Ordering::Acquire);
                loop {
                    if count >= MAX_WAITING_FOLLOWERS {
                        return Err((
                            ErrorCode::Unavailable,
                            "too many concurrent PTY followers".to_owned(),
                        ));
                    }
                    match counter.compare_exchange_weak(
                        count,
                        count + 1,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => break,
                        Err(observed) => count = observed,
                    }
                }
                let _permit = FollowPermit(counter);
                let mut snapshot_args = args.clone();
                snapshot_args["wait_ms"] = json!(0);
                let deadline = Instant::now() + Duration::from_millis(wait_ms);
                loop {
                    let snapshot = self.control(store, name, &snapshot_args)?;
                    let requested = args.get("cursor").and_then(Value::as_u64).unwrap_or(0);
                    if snapshot
                        .get("cursor")
                        .and_then(Value::as_u64)
                        .is_some_and(|cursor| cursor > requested)
                        || snapshot.get("running") == Some(&Value::Bool(false))
                        || Instant::now() >= deadline
                    {
                        return Ok(snapshot);
                    }
                    std::thread::sleep(
                        deadline
                            .saturating_duration_since(Instant::now())
                            .min(Duration::from_millis(25)),
                    );
                }
            }
        }
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
                let now = Instant::now();
                match name {
                    "session.attach" => {
                        issued_token = Some(attach_writer(&mut guardian.writer, now)?);
                    }
                    "session.write" | "session.resize" | "session.detach" | "session.heartbeat" => {
                        let requested = args
                            .get("attachment_token")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                invalid("attachment_token required for terminal writer operations")
                            })?;
                        renew_writer(&mut guardian.writer, requested, now)?;
                    }
                    _ => expire_writer(&mut guardian.writer, now),
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
            return Ok(json!({"lease_active": true, "expires_in_ms": WRITER_LEASE.as_millis()}));
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
        // The guardian marks caller mistakes structurally; anything else
        // (exited, backpressure, lost state) stays transient `unavailable`.
        let code = match response.get("code").and_then(Value::as_str) {
            Some("invalid_argument") => ErrorCode::InvalidArgument,
            _ => ErrorCode::Unavailable,
        };
        return Err((code, reason.to_owned()));
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_lease_is_exclusive_until_it_expires() {
        let start = Instant::now();
        let mut writer = None;
        let first = attach_writer(&mut writer, start).expect("first attach");
        let contested = attach_writer(&mut writer, start + WRITER_LEASE - Duration::from_millis(1));
        assert_eq!(contested.unwrap_err().0, ErrorCode::Conflict);

        // At the deadline the lease lapses: a new writer wins with a new token
        // and the old token is revoked, not silently revived.
        let second = attach_writer(&mut writer, start + WRITER_LEASE).expect("attach after expiry");
        assert_ne!(first, second);
        let revoked = renew_writer(&mut writer, &first, start + WRITER_LEASE);
        assert_eq!(revoked.unwrap_err().0, ErrorCode::Conflict);
        renew_writer(&mut writer, &second, start + WRITER_LEASE).expect("current token");
    }

    #[test]
    fn heartbeats_extend_the_lease_and_expired_tokens_never_renew() {
        let start = Instant::now();
        let mut writer = None;
        let token = attach_writer(&mut writer, start).expect("attach");
        let half = WRITER_LEASE / 2;
        // Renewing every half lease keeps the writer alive well past one lease.
        for step in 1..=4 {
            renew_writer(&mut writer, &token, start + half * step).expect("renewal");
        }
        let last_renewal = start + half * 4;
        assert!(attach_writer(
            &mut writer,
            last_renewal + WRITER_LEASE - Duration::from_millis(1)
        )
        .is_err());

        // Once it lapses, the same token cannot resurrect it.
        let lapsed = renew_writer(&mut writer, &token, last_renewal + WRITER_LEASE);
        assert_eq!(lapsed.unwrap_err().0, ErrorCode::Conflict);
        assert!(writer.is_none(), "expired lease must be dropped");
    }

    #[test]
    fn foreign_tokens_never_extend_a_live_lease() {
        let start = Instant::now();
        let mut writer = None;
        let token = attach_writer(&mut writer, start).expect("attach");
        for attempt in 1..=100u32 {
            let forged = renew_writer(
                &mut writer,
                &format!("forged-{attempt}"),
                start + Duration::from_secs(1),
            );
            assert_eq!(forged.unwrap_err().0, ErrorCode::Conflict);
        }
        // The forged attempts did not move the deadline.
        let after = start + WRITER_LEASE;
        assert_eq!(
            renew_writer(&mut writer, &token, after).unwrap_err().0,
            ErrorCode::Conflict
        );
    }
}
