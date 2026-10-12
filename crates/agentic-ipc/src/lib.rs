#![forbid(unsafe_code)]

use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
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

    Command { name: String, args: Value },

    Ok { data: Value },

    Err { message: String },
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
}

impl SocketOwnership {
    fn new(path: &Path) -> std::io::Result<Self> {
        let meta = std::fs::symlink_metadata(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            device: meta.dev(),
            inode: meta.ino(),
        })
    }
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

/// Kernel-attested identity of the Unix socket peer. Never populated from
/// JSON command arguments or client-supplied identity claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerIdentity {
    pub uid: u32,
    pub gid: u32,
    pub pid: Option<u32>,
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
    mut shutdown_requested: watch::Receiver<bool>,
) -> Result<()> {
    let path = socket_path.as_ref().to_path_buf();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Bind without unlinking: an existing socket (active OR stale) must not be
    // silently replaced. Stale sockets require explicit operator recovery.
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let ownership = SocketOwnership::new(&path)?;

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
                        // Fail closed if the kernel cannot identify the peer or
                        // its UID differs from the socket owner's UID.
                        // This is an explicitly single-user IPC boundary.
                        let peer = match stream.peer_cred() {
                            Ok(peer) if peer.uid() == server_uid => {
                                tracing::trace!(peer_uid = peer.uid(), peer_pid = ?peer.pid(), "authorized local IPC peer");
                                PeerIdentity {
                                    uid: peer.uid(),
                                    gid: peer.gid(),
                                    pid: peer.pid(),
                                }
                            }
                            Ok(peer) => {
                                tracing::warn!(peer_uid = peer.uid(), server_uid, "rejecting IPC peer from another UID");
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
        // Permit travels into the blocking closure, so cancellation cannot
        // free capacity while a synchronous SQLite request is still running.
        let permit = Arc::clone(&blocking)
            .acquire_owned()
            .await
            .map_err(|_| IpcError::Closed("handler pool shut down".into()))?;
        let handler_ref = Arc::clone(&handler);
        let request_for_handler = request.clone();
        let response = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            handler_ref(&request_for_handler, &peer)
        })
        .await?;
        tokio::time::timeout(IO_DEADLINE, write_frame(&mut stream, &response))
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
        tokio::time::timeout(IO_DEADLINE, write_frame(&mut self.stream, &request))
            .await
            .map_err(|_| IpcError::Timeout)??;
        let response = tokio::time::timeout(IO_DEADLINE, read_frame(&mut self.stream))
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

#[must_use]
pub fn socket_path(runtime_dir: impl AsRef<Path>, service: &str) -> PathBuf {
    runtime_dir.as_ref().join(format!("{service}.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
            }
            _ => panic!("expected authenticated IPC response"),
        }
        shutdown.send(true).unwrap();
        server.await.unwrap().unwrap();
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

    #[test]
    fn socket_path_is_deterministic() {
        let p = socket_path("/tmp/agentic", "flowd");
        assert_eq!(p, PathBuf::from("/tmp/agentic/flowd.sock"));
    }
}
