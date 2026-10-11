//! Minimal, versionable local IPC for Flowerly OS daemons.
//!
//! The protocol is deliberately simple: a length-prefixed UTF-8 JSON body over
//! a Unix domain socket. Length is a little-endian `u32`. Keeping framing
//! trivial means the wire format is inspectable with `nc`/`socat` and easy to
//! evolve through schema versioning on the JSON objects themselves.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

/// Maximum accepted frame size (8 MiB). Guards against runaway allocations.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Protocol version carried by every request/response.
pub const PROTOCOL_VERSION: u32 = 1;

/// Errors produced by the IPC layer.
#[derive(Debug, Error)]
pub enum IpcError {
    /// I/O failure.
    #[error("ipc io error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON (de)serialization failure.
    #[error("ipc serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    /// Received frame violates the framing contract.
    #[error("invalid frame: {0}")]
    InvalidFrame(String),
    /// The connection was closed by the peer mid-frame.
    #[error("connection closed: {0}")]
    Closed(String),
}

/// Result alias for the IPC layer.
pub type Result<T> = std::result::Result<T, IpcError>;

/// Versioned request envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// Protocol version (must equal [`PROTOCOL_VERSION`]).
    pub version: u32,
    /// Correlation id echoed back by the server.
    pub id: String,
    /// The payload.
    #[serde(flatten)]
    pub payload: Payload,
}

/// A request or response body.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Payload {
    /// Liveness probe.
    Ping,
    /// Request structured health/status.
    Status,
    /// Ordered graceful shutdown.
    Shutdown,
    /// An operation addressed to the service.
    Command {
        /// Operation name.
        name: String,
        /// Operation arguments.
        args: Value,
    },
    /// Positive outcome carrying data.
    Ok {
        /// Result data.
        data: Value,
    },
    /// Failed outcome carrying a message.
    Err {
        /// Human-readable error.
        message: String,
    },
}

impl Envelope {
    /// Create a fresh envelope for `payload`.
    #[must_use]
    pub fn new(payload: Payload) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id: uuid::Uuid::new_v4().simple().to_string(),
            payload,
        }
    }
}

/// Write an envelope as a length-prefixed JSON frame.
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

/// Read one envelope from the stream.
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

/// Request handler signature. Handlers are synchronous by design: status
/// queries touch local state only and must never depend on a network or an AI
/// provider.
pub type Handler = dyn Fn(&Envelope) -> Envelope + Send + Sync;

/// Serve requests on the given socket path until a shutdown signal arrives.
///
/// The handler must be cheaply clonable-free: it is shared behind an [`Arc`]
/// across all accepted connections. The socket file is removed on clean
/// shutdown.
pub async fn serve(socket_path: impl AsRef<Path>, handler: Arc<Handler>) -> Result<()> {
    let path = socket_path.as_ref().to_path_buf();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(&path);

    let listener = UnixListener::bind(&path)?;
    tracing::info!(socket = %path.display(), "ipc server listening");

    let mut shutdown = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            _signal = shutdown.recv() => {
                tracing::info!("received SIGTERM, shutting down");
                break;
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _addr)) => {
                        let handler_ref = Arc::clone(&handler);
                        tokio::spawn(async move {
                            if let Err(err) = handle_connection(stream, handler_ref).await {
                                tracing::debug!(error = %err, "ipc connection ended");
                            }
                        });
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "accept failed");
                    }
                }
            }
        }
    }

    let _ = std::fs::remove_file(&path);
    Ok(())
}

async fn handle_connection(mut stream: UnixStream, handler: Arc<Handler>) -> Result<()> {
    loop {
        let request = match read_frame(&mut stream).await {
            Ok(frame) => frame,
            Err(IpcError::Closed(_)) => return Ok(()),
            Err(err) => return Err(err),
        };
        let response = handler(&request);
        write_frame(&mut stream, &response).await?;
        if matches!(request.payload, Payload::Shutdown) {
            return Ok(());
        }
    }
}

/// A minimal client handle over a single connection.
pub struct Client {
    stream: UnixStream,
}

impl Client {
    /// Connect to a running service.
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self> {
        let stream = UnixStream::connect(path.as_ref()).await?;
        Ok(Self { stream })
    }

    /// Send one request and await the response.
    pub async fn request(&mut self, payload: Payload) -> Result<Envelope> {
        let request = Envelope::new(payload);
        write_frame(&mut self.stream, &request).await?;
        read_frame(&mut self.stream).await
    }
}

/// One-shot call: open a connection, send the request, return the response.
pub async fn call(path: impl AsRef<Path>, payload: Payload) -> Result<Envelope> {
    let mut client = Client::connect(path).await?;
    client.request(payload).await
}

/// Path of the socket for a named service under a runtime root.
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
        assert_eq!(response.id, response.id.clone());
        assert!(matches!(response.payload, Payload::Ok { .. }));

        server.abort();
    }

    #[test]
    fn socket_path_is_deterministic() {
        let p = socket_path("/tmp/agentic", "flowd");
        assert_eq!(p, PathBuf::from("/tmp/agentic/flowd.sock"));
    }
}
