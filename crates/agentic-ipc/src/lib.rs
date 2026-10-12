#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

pub const PROTOCOL_VERSION: u32 = 1;
const IO_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

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

pub type Handler = dyn Fn(&Envelope) -> Envelope + Send + Sync;

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

pub struct Client {
    stream: UnixStream,
}

impl Client {
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self> {
        let stream = tokio::time::timeout(IO_DEADLINE, UnixStream::connect(path.as_ref()))
            .await.map_err(|_| IpcError::Timeout)??;
        Ok(Self { stream })
    }

    pub async fn request(&mut self, payload: Payload) -> Result<Envelope> {
        let request = Envelope::new(payload);
        tokio::time::timeout(IO_DEADLINE, write_frame(&mut self.stream, &request))
            .await.map_err(|_| IpcError::Timeout)??;
        let response = tokio::time::timeout(IO_DEADLINE, read_frame(&mut self.stream))
            .await.map_err(|_| IpcError::Timeout)??;
        if response.id != request.id {
            return Err(IpcError::InvalidFrame("response correlation id mismatch".into()));
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

    #[test]
    fn socket_path_is_deterministic() {
        let p = socket_path("/tmp/agentic", "flowd");
        assert_eq!(p, PathBuf::from("/tmp/agentic/flowd.sock"));
    }
}
