#![forbid(unsafe_code)]

use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{watch, Semaphore};
use tokio::task::JoinSet;

pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

pub const PROTOCOL_VERSION: u32 = 1;
const IO_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);
/// Total response budget after a complete frame is received: includes queueing,
/// the blocking handler and socket write, not a fresh ten seconds per stage.
pub const REQUEST_DEADLINE: std::time::Duration = IO_DEADLINE;
/// Maximum concurrently connected IPC clients per daemon.
pub const MAX_CONNECTIONS: usize = 64;
/// Limits synchronous handlers (including SQLite) occupying blocking threads.
pub const MAX_BLOCKING_HANDLERS: usize = 8;

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("ipc io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("ipc serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("invalid frame: {0}")]
    InvalidFrame(String),

    #[error("connection closed: {0}")]
    Closed(String),

    #[error("ipc operation timed out")]
    Timeout,

    #[error("ipc handler task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

pub type Result<T> = std::result::Result<T, IpcError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub version: u32,

    pub id: String,

    #[serde(flatten)]
    pub payload: Payload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Payload {
    Ping,

    Status,

    Shutdown,

    Command {
        name: String,
        args: Value,
    },

    Ok {
        data: Value,
    },

    Err {
        message: String,

        /// Stable machine-readable error classification. Legacy peers omit it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<ErrorCode>,
    },
}

/// Stable error classes shared by all Flowerly IPC services.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidArgument,
    NotFound,
    Conflict,
    PermissionDenied,
    Unavailable,
    Internal,
}

impl Envelope {
    #[must_use]
    pub fn new(payload: Payload) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id: uuid::Uuid::new_v4().simple().to_string(),
            payload,
        }
    }
}

pub async fn write_frame(
    stream: &mut (impl AsyncWriteExt + Unpin),
    envelope: &Envelope,
) -> Result<()> {
    let body = serde_json::to_vec(envelope)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(IpcError::InvalidFrame(format!(
            "frame too large ({} bytes)",
            body.len()
        )));
    }
    let len = u32::try_from(body.len())
        .map_err(|_| IpcError::InvalidFrame("frame length overflow".into()))?;
    stream.write_all(&len.to_le_bytes()).await?;
    stream.write_all(&body).await?;
    stream.flush().await?;
    Ok(())
}

pub async fn read_frame(stream: &mut (impl AsyncReadExt + Unpin)) -> Result<Envelope> {
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .map_err(|e| IpcError::Closed(format!("reading length prefix: {e}")))?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(IpcError::InvalidFrame(format!(
            "declared length {len} exceeds limit {MAX_FRAME_BYTES}"
        )));
    }
    let mut body = vec![0u8; len];
    stream
        .read_exact(&mut body)
        .await
        .map_err(|e| IpcError::Closed(format!("reading body: {e}")))?;
    let envelope: Envelope = serde_json::from_slice(&body).map_err(IpcError::Serde)?;
    if envelope.version != PROTOCOL_VERSION {
        return Err(IpcError::InvalidFrame(format!(
            "unsupported protocol version {}",
            envelope.version
        )));
    }
    Ok(envelope)
}

struct SocketOwnership {
    path: PathBuf,
    device: u64,
    inode: u64,
    // Released only after the socket is removed (fields drop after `drop`).
    _instance: Option<InstanceLock>,
}

impl SocketOwnership {
    fn new(path: &Path) -> std::io::Result<Self> {
        let meta = std::fs::symlink_metadata(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            device: meta.dev(),
            inode: meta.ino(),
            _instance: None,
        })
    }
}

/// Exclusive per-socket instance lock (`<socket>.lock`, held for the
/// server's lifetime). Every Flowerly IPC server takes it before binding, so
/// while it is held no cooperating instance can bind, probe or unlink the
/// same pathname. The lock file is never deleted: unlinking it would let two
/// instances lock different inodes for the same socket.
struct InstanceLock {
    file: std::fs::File,
}

impl InstanceLock {
    fn acquire(socket: &Path) -> std::io::Result<Self> {
        let mut name = socket.as_os_str().to_owned();
        name.push(".lock");
        let path = PathBuf::from(name);
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            if !meta.file_type().is_file() {
                return Err(std::io::Error::other(format!(
                    "instance lock {} is not a regular file",
                    path.display()
                )));
            }
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)?;
        let opened = file.metadata()?;
        let linked = std::fs::symlink_metadata(&path)?;
        if (opened.dev(), opened.ino()) != (linked.dev(), linked.ino())
            || opened.mode() & 0o077 != 0
        {
            return Err(std::io::Error::other(format!(
                "instance lock {} was replaced or is accessible to other users",
                path.display()
            )));
        }
        match file.try_lock() {
            Ok(()) => Ok(Self { file }),
            Err(std::fs::TryLockError::WouldBlock) => Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!(
                    "another instance holds {}; refusing to bind {}",
                    path.display(),
                    socket.display()
                ),
            )),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }

    fn owner_uid(&self) -> std::io::Result<u32> {
        Ok(self.file.metadata()?.uid())
    }
}

/// Remove a leftover socket only when it is provably stale. The caller holds
/// the instance lock, so no cooperating server can bind the path meanwhile.
/// Anything that is not a same-owner socket whose listener refuses
/// connections and whose inode is unchanged fails closed.
fn remove_verified_stale_socket(path: &Path, owner_uid: u32) -> std::io::Result<()> {
    let refuse = |reason: &str| {
        Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!("refusing to replace {}: {reason}", path.display()),
        ))
    };
    let before = std::fs::symlink_metadata(path)?;
    if !before.file_type().is_socket() {
        return refuse("path exists and is not a Unix socket");
    }
    if before.uid() != owner_uid {
        return refuse("socket is owned by another user");
    }
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => return refuse("a listener is accepting connections"),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
        Err(error) => return refuse(&format!("liveness probe was inconclusive: {error}")),
    }
    let after = std::fs::symlink_metadata(path)?;
    if (after.dev(), after.ino()) != (before.dev(), before.ino()) {
        return refuse("socket was replaced during verification");
    }
    std::fs::remove_file(path)?;
    tracing::warn!(socket = %path.display(), "removed verified stale IPC socket");
    Ok(())
}

/// Bind under the instance lock, recovering only a verified stale socket.
fn bind_owned(path: &Path) -> std::io::Result<(UnixListener, InstanceLock)> {
    let lock = InstanceLock::acquire(path)?;
    let listener = match UnixListener::bind(path) {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            remove_verified_stale_socket(path, lock.owner_uid()?)?;
            UnixListener::bind(path)?
        }
        Err(error) => return Err(error),
    };
    Ok((listener, lock))
}

impl Drop for SocketOwnership {
    fn drop(&mut self) {
        // A replacement at the same pathname belongs to another instance.
        if let Ok(meta) = std::fs::symlink_metadata(&self.path) {
            if meta.file_type().is_socket() && meta.dev() == self.device && meta.ino() == self.inode
            {
                if let Err(error) = std::fs::remove_file(&self.path) {
                    tracing::warn!(socket = %self.path.display(), %error, "socket cleanup failed");
                }
            }
        }
    }
}

pub type Handler = dyn Fn(&Envelope) -> Envelope + Send + Sync;

/// Kernel-attested identity of the Unix socket peer, captured once at
/// `connect()` (`SO_PEERCRED` plus `SO_PEERGROUPS`) and reused for every
/// request on that connection. Never populated from JSON command arguments
/// or client-supplied identity claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    pub uid: u32,
    pub gid: u32,
    pub pid: Option<u32>,
    /// Supplementary groups; the primary `gid` is not repeated here.
    pub groups: Arc<[u32]>,
    /// The peer runs as the service account that owns the socket.
    pub service_account: bool,
}

impl PeerIdentity {
    /// The kernel-attested caller of requests on this connection (#225).
    #[must_use]
    pub fn caller(&self) -> crate::types::Caller {
        crate::types::Caller::new(self.uid)
    }

    /// Primary or supplementary membership of `gid`.
    #[must_use]
    pub fn in_group(&self, gid: u32) -> bool {
        self.gid == gid || self.groups.contains(&gid)
    }
}

/// Who may connect and who may administer, as resolved Linux gids.
///
/// The default policy admits only the service account and grants no
/// administrative authority to anyone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccessPolicy {
    /// Members may connect and run User commands.
    pub users_gid: Option<u32>,
    /// Members may run Admin commands. The service account is not
    /// implicitly a member.
    pub admin_gid: Option<u32>,
}

impl AccessPolicy {
    /// The service account and members of the users group. Admin group
    /// membership alone never admits a peer: it authorizes Admin commands
    /// for a peer that is already admitted, so without a users group the
    /// socket is service-account-only at the kernel check too (#225).
    #[must_use]
    pub fn admits(&self, peer: &PeerIdentity) -> bool {
        peer.service_account || self.users_gid.is_some_and(|gid| peer.in_group(gid))
    }

    /// Kernel-attested membership of the configured admin group.
    #[must_use]
    pub fn is_admin(&self, peer: &PeerIdentity) -> bool {
        self.admin_gid.is_some_and(|gid| peer.in_group(gid))
    }
}

pub type AuthenticatedHandler = dyn Fn(&Envelope, &PeerIdentity) -> Envelope + Send + Sync;

pub async fn serve(socket_path: impl AsRef<Path>, handler: Arc<Handler>) -> Result<()> {
    let (_tx, rx) = watch::channel(false);
    serve_with_shutdown(socket_path, handler, rx).await
}

/// Stop accepting new clients when shutdown is requested; drain accepted
/// connections up to a bounded deadline before removing our socket.
pub async fn serve_with_shutdown(
    socket_path: impl AsRef<Path>,
    handler: Arc<Handler>,
    shutdown_requested: watch::Receiver<bool>,
) -> Result<()> {
    let wrapped: Arc<AuthenticatedHandler> = Arc::new(move |request, _peer| handler(request));
    serve_with_shutdown_authenticated(socket_path, wrapped, shutdown_requested).await
}

/// Accept only same-UID Unix peers and preserve kernel-attested identity for
/// each request throughout the lifetime of its accepted connection.
pub async fn serve_with_shutdown_authenticated(
    socket_path: impl AsRef<Path>,
    handler: Arc<AuthenticatedHandler>,
    shutdown_requested: watch::Receiver<bool>,
) -> Result<()> {
    serve_with_policy(
        socket_path,
        AccessPolicy::default(),
        handler,
        shutdown_requested,
    )
    .await
}

/// Accept the service account and members of the policy's groups. With a
/// users group the socket is group-owned by it with mode 0660; otherwise it
/// stays 0600. The kernel identity of each peer, including supplementary
/// groups, is checked again at accept time, so filesystem access alone is
/// never sufficient.
pub async fn serve_with_policy(
    socket_path: impl AsRef<Path>,
    policy: AccessPolicy,
    handler: Arc<AuthenticatedHandler>,
    mut shutdown_requested: watch::Receiver<bool>,
) -> Result<()> {
    let path = socket_path.as_ref().to_path_buf();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Never blindly unlink: a live socket, foreign file or another instance's
    // socket fails closed. Only a verified stale socket is recovered.
    let (listener, instance) = bind_owned(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let mut ownership = SocketOwnership::new(&path)?;
    if let Some(gid) = policy.users_gid {
        // Change the group while the mode is still 0600, then grant group
        // access, so no other group can ever reach the socket.
        std::os::unix::fs::lchown(&path, None, Some(gid))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660))?;
    }
    ownership._instance = Some(instance);

    // The socket owner is the daemon's effective filesystem identity.
    // Credentials come from the kernel, never request JSON.
    let server_uid = std::fs::metadata(&path)?.uid();
    tracing::info!(socket = %path.display(), "ipc server listening");

    let mut shutdown = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let blocking_handlers = Arc::new(Semaphore::new(MAX_BLOCKING_HANDLERS));
    let mut connections_tasks = JoinSet::new();

    loop {
        tokio::select! {
            changed = shutdown_requested.changed() => {
                if changed.is_err() || *shutdown_requested.borrow() {
                    tracing::info!("IPC shutdown requested; draining connections");
                    break;
                }
            }
            _signal = shutdown.recv() => {
                tracing::info!("received SIGTERM, shutting down");
                break;
            }
            finished = connections_tasks.join_next(), if !connections_tasks.is_empty() => {
                if let Some(Err(error)) = finished {
                    tracing::warn!(%error, "IPC connection task failed");
                }
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _addr)) => {
                        // Fail closed if the kernel cannot identify the peer,
                        // its groups cannot be read, or the policy does not
                        // admit it. Credentials are the connect-time snapshot.
                        let peer = match peer_identity(&stream, server_uid) {
                            Ok(peer) if policy.admits(&peer) => {
                                tracing::trace!(peer_uid = peer.uid, peer_pid = ?peer.pid, "authorized local IPC peer");
                                peer
                            }
                            Ok(peer) => {
                                tracing::warn!(peer_uid = peer.uid, server_uid, "rejecting IPC peer outside the access policy");
                                continue;
                            }
                            Err(error) => {
                                tracing::warn!(%error, "cannot authenticate IPC peer");
                                continue;
                            }
                        };
                        // Refuse excess clients before spawning a task. A slow
                        // or silent client cannot consume unbounded memory.
                        if let Ok(permit) = Arc::clone(&connections).try_acquire_owned() {
                            let handler_ref = Arc::clone(&handler);
                            let blocking = Arc::clone(&blocking_handlers);
                            connections_tasks.spawn(async move {
                                let _connection = permit;
                                if let Err(err) = handle_connection(stream, handler_ref, blocking, peer).await {
                                    tracing::debug!(error = %err, "ipc connection ended");
                                }
                            });
                        } else {
                            tracing::warn!("ipc connection limit reached; refusing client");
                        }
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "accept failed");
                    }
                }
            }
        }
    }

    // Stop accepting new connections before draining workers. The task that
    // requested shutdown has time to flush its OK response to the client.
    drop(listener);
    if tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(result) = connections_tasks.join_next().await {
            if let Err(error) = result {
                tracing::warn!(%error, "IPC connection task failed on shutdown");
            }
        }
    })
    .await
    .is_err()
    {
        tracing::warn!("IPC drain deadline expired; aborting remaining connections");
        connections_tasks.abort_all();
        while connections_tasks.join_next().await.is_some() {}
    }
    drop(ownership);
    Ok(())
}

/// Kernel credentials of an accepted peer. Any failure, including an
/// unreadable supplementary group list, refuses the connection.
fn peer_identity(stream: &UnixStream, server_uid: u32) -> std::io::Result<PeerIdentity> {
    let credentials = stream.peer_cred()?;
    let groups = flowerly_peercred::peer_groups(stream)?;
    Ok(PeerIdentity {
        uid: credentials.uid(),
        gid: credentials.gid(),
        pid: credentials.pid().and_then(|pid| u32::try_from(pid).ok()),
        groups: groups.into(),
        service_account: credentials.uid() == server_uid,
    })
}

/// Where a request's deadline expired. The distinction matters to callers:
/// a request that never left the queue was not executed, while one whose
/// handler was still running may commit later (see ipc-mutation-outcomes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeadlineStage {
    Queued,
    Running,
}

fn unavailable_response(
    request: &Envelope,
    stage: DeadlineStage,
    elapsed: std::time::Duration,
) -> Envelope {
    let ms = elapsed.as_millis();
    let message = match stage {
        DeadlineStage::Queued => format!(
            "request deadline exceeded after {ms} ms while queued for a handler; the request was not executed"
        ),
        DeadlineStage::Running => format!(
            "request deadline exceeded after {ms} ms while the handler was running; the outcome is unknown"
        ),
    };
    Envelope {
        version: PROTOCOL_VERSION,
        id: request.id.clone(),
        payload: Payload::Err {
            message,
            code: Some(ErrorCode::Unavailable),
        },
    }
}

/// Stable label for request diagnostics. Command arguments are never logged.
fn request_label(payload: &Payload) -> String {
    match payload {
        Payload::Command { name, .. } => name.clone(),
        other => serde_json::to_value(other)
            .ok()
            .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| "unknown".to_owned()),
    }
}

/// Requests slower than this are logged at warn level even when they succeed.
const SLOW_REQUEST: std::time::Duration = std::time::Duration::from_secs(1);

/// One structured event per request: correlation ID, request label, peer UID,
/// time queued for a handler slot, handler time, total time and outcome.
fn log_request(
    request: &Envelope,
    response: &Envelope,
    peer: &PeerIdentity,
    queue: std::time::Duration,
    handler: Option<std::time::Duration>,
    total: std::time::Duration,
    deadline: Option<DeadlineStage>,
) {
    let code = match &response.payload {
        Payload::Err { code, .. } => Some(code.unwrap_or(ErrorCode::Internal)),
        _ => None,
    };
    let outcome = match (deadline, code) {
        (Some(DeadlineStage::Queued), _) => "deadline_queued",
        (Some(DeadlineStage::Running), _) => "deadline_running",
        (None, Some(_)) => "error",
        (None, None) => "ok",
    };
    let label = request_label(&request.payload);
    let queue_ms = queue.as_millis() as u64;
    let handler_ms = handler.map(|elapsed| elapsed.as_millis() as u64);
    let total_ms = total.as_millis() as u64;
    if deadline.is_some() || total >= SLOW_REQUEST {
        tracing::warn!(
            request_id = %request.id, request = %label, peer_uid = peer.uid,
            queue_ms, handler_ms, total_ms, outcome, code = ?code,
            "ipc request"
        );
    } else {
        tracing::debug!(
            request_id = %request.id, request = %label, peer_uid = peer.uid,
            queue_ms, handler_ms, total_ms, outcome, code = ?code,
            "ipc request"
        );
    }
}

async fn handle_connection(
    mut stream: UnixStream,
    handler: Arc<AuthenticatedHandler>,
    blocking: Arc<Semaphore>,
    peer: PeerIdentity,
) -> Result<()> {
    loop {
        let request = match tokio::time::timeout(IO_DEADLINE, read_frame(&mut stream)).await {
            Ok(Ok(frame)) => frame,
            Ok(Err(IpcError::Closed(_))) => return Ok(()),
            Ok(Err(err)) => return Err(err),
            Err(_) => return Err(IpcError::Timeout),
        };
        // Set the clock before waiting for a handler slot. A busy SQLite
        // writer can keep a synchronous worker occupied, but it must not
        // extend the IPC caller's budget or consume another async worker.
        let received = tokio::time::Instant::now();
        let deadline = received + REQUEST_DEADLINE;
        // Reserve a short response window so expired queued work can still
        // return a correlated, machine-readable Unavailable response.
        let handler_deadline = deadline - std::time::Duration::from_millis(250);
        let queue;
        let mut handler_elapsed = None;
        let mut expired = None;
        let response =
            match tokio::time::timeout_at(handler_deadline, Arc::clone(&blocking).acquire_owned())
                .await
            {
                Ok(Ok(permit)) => {
                    queue = received.elapsed();
                    let started = tokio::time::Instant::now();
                    // The permit remains with the worker even when the request
                    // expires; do not count a still-running SQLite write as free.
                    let handler_ref = Arc::clone(&handler);
                    let request_for_handler = request.clone();
                    let peer = peer.clone();
                    let task = tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        handler_ref(&request_for_handler, &peer)
                    });
                    let outcome = tokio::time::timeout_at(handler_deadline, task).await;
                    handler_elapsed = Some(started.elapsed());
                    match outcome {
                        Ok(Ok(response)) => response,
                        Ok(Err(error)) => return Err(IpcError::Task(error)),
                        Err(_) => {
                            expired = Some(DeadlineStage::Running);
                            unavailable_response(
                                &request,
                                DeadlineStage::Running,
                                received.elapsed(),
                            )
                        }
                    }
                }
                Ok(Err(_)) => return Err(IpcError::Closed("handler pool shut down".into())),
                Err(_) => {
                    queue = received.elapsed();
                    expired = Some(DeadlineStage::Queued);
                    unavailable_response(&request, DeadlineStage::Queued, queue)
                }
            };
        log_request(
            &request,
            &response,
            &peer,
            queue,
            handler_elapsed,
            received.elapsed(),
            expired,
        );
        // Responses that themselves cannot be written before the deadline
        // close the connection. The client has the same bounded budget.
        tokio::time::timeout_at(deadline, write_frame(&mut stream, &response))
            .await
            .map_err(|_| IpcError::Timeout)??;
        if matches!(request.payload, Payload::Shutdown) {
            return Ok(());
        }
    }
}

pub struct Client {
    stream: UnixStream,
}

impl Client {
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self> {
        let stream = tokio::time::timeout(IO_DEADLINE, UnixStream::connect(path.as_ref()))
            .await
            .map_err(|_| IpcError::Timeout)??;
        Ok(Self { stream })
    }

    pub async fn request(&mut self, payload: Payload) -> Result<Envelope> {
        let request = Envelope::new(payload);
        let deadline = tokio::time::Instant::now() + REQUEST_DEADLINE;
        tokio::time::timeout_at(deadline, write_frame(&mut self.stream, &request))
            .await
            .map_err(|_| IpcError::Timeout)??;
        let response = tokio::time::timeout_at(deadline, read_frame(&mut self.stream))
            .await
            .map_err(|_| IpcError::Timeout)??;
        if response.id != request.id {
            return Err(IpcError::InvalidFrame(
                "response correlation id mismatch".into(),
            ));
        }
        Ok(response)
    }
}

pub async fn call(path: impl AsRef<Path>, payload: Payload) -> Result<Envelope> {
    let mut client = Client::connect(path).await?;
    client.request(payload).await
}

/// Blocking variant of [`call`] for bounded worker threads inside daemons
/// that are not inside a Tokio async context. Builds a single-threaded
/// temporary runtime per call so the IPC deadline guarantees still apply.
///
/// # Errors
/// Returns the IPC framing/deadline error on failure.
pub fn call_blocking(path: impl AsRef<Path>, payload: Payload) -> Result<Envelope> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(IpcError::Io)?
        .block_on(call(path, payload))
}

#[must_use]
pub fn socket_path(runtime_dir: impl AsRef<Path>, service: &str) -> PathBuf {
    runtime_dir.as_ref().join(format!("{service}.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn coded_errors_round_trip_and_legacy_errors_remain_readable() {
        let error = Envelope {
            version: PROTOCOL_VERSION,
            id: "request-1".to_owned(),
            payload: Payload::Err {
                code: Some(ErrorCode::Conflict),
                message: "already exists".to_owned(),
            },
        };
        let serialized = serde_json::to_value(&error).unwrap();
        assert_eq!(serialized["code"], "conflict");
        let decoded: Envelope = serde_json::from_value(serialized).unwrap();
        assert!(matches!(
            decoded.payload,
            Payload::Err {
                code: Some(ErrorCode::Conflict),
                ..
            }
        ));

        let legacy = serde_json::json!({
            "version": PROTOCOL_VERSION,
            "id": "request-2",
            "type": "err",
            "message": "old error"
        });
        let decoded: Envelope = serde_json::from_value(legacy).unwrap();
        assert!(matches!(decoded.payload, Payload::Err { code: None, .. }));
    }

    #[test]
    fn envelope_serializes_with_type_tag() {
        let env = Envelope::new(Payload::Ping);
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"type\":\"ping\""));
    }

    #[tokio::test]
    async fn request_response_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.sock");

        let handler = Arc::new(|req: &Envelope| -> Envelope {
            let data = serde_json::json!({ "echo": req.id });
            Envelope {
                version: PROTOCOL_VERSION,
                id: req.id.clone(),
                payload: Payload::Ok { data },
            }
        });

        let server = tokio::spawn(serve(path.clone(), handler));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let mut client = Client::connect(&path).await.expect("connect");
        let response = client
            .request(Payload::Status)
            .await
            .expect("request status");
        assert!(!response.id.is_empty());
        assert!(matches!(response.payload, Payload::Ok { .. }));

        server.abort();
    }

    #[tokio::test]
    async fn never_replaces_a_regular_file_or_live_socket() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("safe.sock");
        std::fs::write(&path, "do not delete").unwrap();
        let handler: Arc<Handler> = Arc::new(|req: &Envelope| Envelope {
            version: PROTOCOL_VERSION,
            id: req.id.clone(),
            payload: Payload::Ok { data: Value::Null },
        });
        assert!(serve(&path, Arc::clone(&handler)).await.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "do not delete");
        std::fs::remove_file(&path).unwrap();
        let listener = UnixListener::bind(&path).unwrap();
        assert!(serve(&path, handler).await.is_err());
        assert!(path.exists());
        drop(listener);
    }

    fn ok_handler() -> Arc<Handler> {
        Arc::new(|req: &Envelope| Envelope {
            version: PROTOCOL_VERSION,
            id: req.id.clone(),
            payload: Payload::Ok { data: Value::Null },
        })
    }

    async fn wait_for_ping(path: &Path) {
        for _ in 0..100 {
            if call(path, Payload::Ping).await.is_ok() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("server never answered on {}", path.display());
    }

    #[tokio::test]
    async fn recovers_a_verified_stale_socket() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("stale.sock");
        // A crashed server leaves its pathname behind with no listener.
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_socket());

        let (shutdown, receiver) = watch::channel(false);
        let server = tokio::spawn(serve_with_shutdown(path.clone(), ok_handler(), receiver));
        wait_for_ping(&path).await;
        shutdown.send(true).unwrap();
        server.await.unwrap().unwrap();
        assert!(!path.exists(), "owned socket is removed on shutdown");
    }

    #[tokio::test]
    async fn second_instance_is_refused_while_the_first_holds_the_lock() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("single.sock");
        let (shutdown, receiver) = watch::channel(false);
        let first = tokio::spawn(serve_with_shutdown(path.clone(), ok_handler(), receiver));
        wait_for_ping(&path).await;
        let inode = std::fs::symlink_metadata(&path).unwrap().ino();

        let error = serve(&path, ok_handler()).await.unwrap_err();
        assert!(error.to_string().contains("another instance"), "{error}");
        assert_eq!(std::fs::symlink_metadata(&path).unwrap().ino(), inode);
        wait_for_ping(&path).await;

        shutdown.send(true).unwrap();
        first.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn never_follows_a_symlinked_socket_or_lock() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("elsewhere.sock");
        drop(std::os::unix::net::UnixListener::bind(&target).unwrap());
        let path = dir.path().join("link.sock");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(serve(&path, ok_handler()).await.is_err());
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(target.exists(), "symlink target must not be removed");

        let lock_target = dir.path().join("victim");
        std::fs::write(&lock_target, "keep").unwrap();
        let locked = dir.path().join("locked.sock");
        std::os::unix::fs::symlink(&lock_target, dir.path().join("locked.sock.lock")).unwrap();
        let error = serve(&locked, ok_handler()).await.unwrap_err();
        assert!(error.to_string().contains("not a regular file"), "{error}");
        assert_eq!(std::fs::read_to_string(&lock_target).unwrap(), "keep");
        assert!(!locked.exists());
    }

    #[tokio::test]
    async fn refuses_a_lock_file_other_users_can_open() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("shared.sock");
        let lock = dir.path().join("shared.sock.lock");
        std::fs::write(&lock, "").unwrap();
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o666)).unwrap();
        let error = serve(&path, ok_handler()).await.unwrap_err();
        assert!(error.to_string().contains("other users"), "{error}");
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn cleanup_does_not_remove_a_replacement_socket() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("replace.sock");
        let original = UnixListener::bind(&path).unwrap();
        let guard = SocketOwnership::new(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let replacement = UnixListener::bind(&path).unwrap();
        drop(guard);
        assert!(path.exists());
        drop(replacement);
        drop(original);
    }

    #[tokio::test]
    async fn authenticated_handler_receives_kernel_attested_peer() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("peer-handler.sock");
        let (shutdown, receiver) = watch::channel(false);
        let handler: Arc<AuthenticatedHandler> =
            Arc::new(|request: &Envelope, peer: &PeerIdentity| Envelope {
                version: PROTOCOL_VERSION,
                id: request.id.clone(),
                payload: Payload::Ok {
                    data: serde_json::json!({
                        "uid": peer.uid,
                        "gid": peer.gid,
                        "pid": peer.pid,
                        "groups": peer.groups.to_vec(),
                        "service_account": peer.service_account,
                    }),
                },
            });
        let server = tokio::spawn(serve_with_shutdown_authenticated(
            path.clone(),
            handler,
            receiver,
        ));
        for _ in 0..50 {
            if path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let expected_uid = std::fs::metadata(&path).unwrap().uid();
        let response = call(&path, Payload::Ping).await.unwrap();
        match response.payload {
            Payload::Ok { data } => {
                assert_eq!(data["uid"], expected_uid);
                assert_eq!(data["pid"], std::process::id());
                assert_eq!(data["service_account"], true);
                let own_groups = flowerly_peercred::peer_groups(
                    &std::os::unix::net::UnixStream::pair().unwrap().0,
                )
                .unwrap();
                assert_eq!(data["groups"], serde_json::json!(own_groups));
            }
            _ => panic!("expected authenticated IPC response"),
        }
        shutdown.send(true).unwrap();
        server.await.unwrap().unwrap();
    }

    fn peer(uid: u32, gid: u32, groups: &[u32], service_account: bool) -> PeerIdentity {
        PeerIdentity {
            uid,
            gid,
            pid: None,
            groups: groups.into(),
            service_account,
        }
    }

    #[test]
    fn access_policy_admits_only_the_service_account_and_group_members() {
        let default = AccessPolicy::default();
        assert!(default.admits(&peer(10, 10, &[], true)));
        assert!(!default.admits(&peer(11, 50, &[51], false)));
        assert!(!default.is_admin(&peer(10, 10, &[], true)));

        let groups = AccessPolicy {
            users_gid: Some(50),
            admin_gid: Some(51),
        };
        assert!(groups.admits(&peer(11, 50, &[], false)), "primary gid");
        assert!(groups.admits(&peer(12, 12, &[50], false)), "supplementary");
        assert!(!groups.admits(&peer(13, 13, &[51], false)), "admin only");
        assert!(
            groups.admits(&peer(16, 16, &[50, 51], false)),
            "admin and users"
        );
        assert!(!groups.admits(&peer(14, 14, &[49, 52], false)));
        let admin_only = AccessPolicy {
            users_gid: None,
            admin_gid: Some(51),
        };
        assert!(admin_only.admits(&peer(10, 10, &[51], true)));
        assert!(
            !admin_only.admits(&peer(17, 51, &[51], false)),
            "service account only"
        );
        assert!(groups.is_admin(&peer(13, 13, &[51], false)));
        assert!(groups.is_admin(&peer(15, 51, &[], false)));
        assert!(!groups.is_admin(&peer(10, 10, &[50], true)));
    }

    async fn wait_for_socket(path: &Path) {
        for _ in 0..100 {
            if path.exists() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("socket {} never appeared", path.display());
    }

    #[tokio::test]
    async fn socket_is_private_without_a_users_group_and_group_owned_with_one() {
        let dir = tempdir().unwrap();
        let probe = dir.path().join("gid-probe");
        std::fs::write(&probe, "").unwrap();
        let own_gid = std::fs::metadata(&probe).unwrap().gid();
        for (name, policy, mode) in [
            ("private.sock", AccessPolicy::default(), 0o600),
            (
                "shared.sock",
                AccessPolicy {
                    users_gid: Some(own_gid),
                    admin_gid: None,
                },
                0o660,
            ),
        ] {
            let path = dir.path().join(name);
            let (shutdown, receiver) = watch::channel(false);
            let handler: Arc<AuthenticatedHandler> =
                Arc::new(|request: &Envelope, _peer: &PeerIdentity| Envelope {
                    version: PROTOCOL_VERSION,
                    id: request.id.clone(),
                    payload: Payload::Ok { data: Value::Null },
                });
            let server = tokio::spawn(serve_with_policy(path.clone(), policy, handler, receiver));
            wait_for_socket(&path).await;
            let meta = std::fs::metadata(&path).unwrap();
            assert_eq!(meta.mode() & 0o777, mode, "{name}");
            assert_eq!(meta.gid(), own_gid, "{name}");
            assert!(matches!(
                call(&path, Payload::Ping).await.unwrap().payload,
                Payload::Ok { .. }
            ));
            shutdown.send(true).unwrap();
            server.await.unwrap().unwrap();
        }
    }

    #[tokio::test]
    async fn kernel_peer_credentials_match_socket_owner_for_same_user() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("credentials.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server_uid = std::fs::metadata(&path).unwrap().uid();
        let client = UnixStream::connect(&path).await.unwrap();
        let (accepted, _) = listener.accept().await.unwrap();
        let peer = accepted.peer_cred().expect("kernel peer credentials");
        assert_eq!(peer.uid(), server_uid);
        assert_eq!(peer.uid(), client.peer_cred().unwrap().uid());
    }

    #[tokio::test]
    async fn silent_client_times_out_without_blocking_other_clients() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bounded.sock");
        let handler: Arc<Handler> = Arc::new(|req: &Envelope| Envelope {
            version: PROTOCOL_VERSION,
            id: req.id.clone(),
            payload: Payload::Ok { data: Value::Null },
        });
        let server = tokio::spawn(serve(path.clone(), handler));
        for _ in 0..50 {
            if path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let mut silent = Client::connect(&path).await.unwrap();
        silent.stream.write_all(&[100, 0, 0, 0]).await.unwrap();
        let healthy = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            call(&path, Payload::Ping),
        )
        .await
        .expect("healthy client must not block")
        .unwrap();
        assert!(matches!(healthy.payload, Payload::Ok { .. }));
        drop(silent);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn queued_request_receives_unavailable_within_total_budget() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = tempdir().unwrap();
        let path = dir.path().join("queue-deadline.sock");
        let started = Arc::new(AtomicUsize::new(0));
        let worker_count = Arc::clone(&started);
        let handler: Arc<Handler> = Arc::new(move |req: &Envelope| {
            let n = worker_count.fetch_add(1, Ordering::SeqCst);
            if n < MAX_BLOCKING_HANDLERS {
                std::thread::sleep(REQUEST_DEADLINE + std::time::Duration::from_secs(2));
            }
            Envelope {
                version: PROTOCOL_VERSION,
                id: req.id.clone(),
                payload: Payload::Ok { data: Value::Null },
            }
        });
        let (shutdown, rx) = watch::channel(false);
        let server = tokio::spawn(serve_with_shutdown(path.clone(), handler, rx));
        for _ in 0..100 {
            if path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let mut requests = Vec::new();
        for _ in 0..MAX_BLOCKING_HANDLERS {
            let socket = path.clone();
            requests.push(tokio::spawn(
                async move { call(socket, Payload::Ping).await },
            ));
        }
        // Wait for all slots to be occupied by the first eight handlers.
        for _ in 0..100 {
            if started.load(Ordering::SeqCst) == MAX_BLOCKING_HANDLERS {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(started.load(Ordering::SeqCst), MAX_BLOCKING_HANDLERS);
        let began = std::time::Instant::now();
        let response = call(&path, Payload::Ping).await.unwrap();
        assert!(began.elapsed() <= REQUEST_DEADLINE + std::time::Duration::from_secs(1));
        match &response.payload {
            Payload::Err {
                code: Some(ErrorCode::Unavailable),
                message,
            } => assert!(
                message.contains("while queued") && message.contains("not executed"),
                "{message}"
            ),
            other => panic!("expected queued unavailable, got {other:?}"),
        }
        let mut running_deadlines = 0;
        for request in requests {
            // The blocked handlers exceed the budget. The server answers
            // before the client deadline and must say the outcome is unknown.
            if let Ok(reply) = request.await.unwrap() {
                match &reply.payload {
                    Payload::Err {
                        code: Some(ErrorCode::Unavailable),
                        message,
                    } => {
                        assert!(
                            message.contains("handler was running")
                                && message.contains("outcome is unknown"),
                            "{message}"
                        );
                        running_deadlines += 1;
                    }
                    other => panic!("expected running unavailable, got {other:?}"),
                }
            }
        }
        assert!(
            running_deadlines > 0,
            "no running-handler deadline was reported"
        );
        shutdown.send(true).unwrap();
        server.await.unwrap().unwrap();
    }

    #[test]
    fn request_labels_never_include_command_arguments() {
        let command = Payload::Command {
            name: "flow.create".into(),
            args: serde_json::json!({"name": "secret-project"}),
        };
        assert_eq!(request_label(&command), "flow.create");
        assert_eq!(request_label(&Payload::Ping), "ping");
        assert_eq!(request_label(&Payload::Status), "status");
    }

    #[test]
    fn socket_path_is_deterministic() {
        let p = socket_path("/tmp/agentic", "flowd");
        assert_eq!(p, PathBuf::from("/tmp/agentic/flowd.sock"));
    }
}
