//! Service runner shared by every Flowerly OS daemon.
//!
//! A daemon is a local IPC server with a uniform status contract and graceful
//! shutdown on SIGTERM or an explicit IPC `shutdown` command.

use std::sync::Arc;
use std::time::Instant;

use agentic_ipc::{Envelope, Payload};
use serde_json::Value;
use tracing::Instrument;

use crate::config::ServiceConfig;

/// Immutable facts every service exposes in its status payload.
#[derive(Debug, Clone)]
pub struct ServiceContext {
    /// Service name, e.g. `flowd`.
    pub name: String,
    /// Package version.
    pub version: String,
    /// Operating-system process id.
    pub pid: u32,
    /// When the service started.
    pub started_at: Instant,
}

impl ServiceContext {
    /// Build a context for the current process.
    #[must_use]
    pub fn for_current_process(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            pid: std::process::id(),
            started_at: Instant::now(),
        }
    }

    /// Uptime in whole seconds.
    #[must_use]
    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// The standard status object for this service.
    #[must_use]
    pub fn status(&self, extra: Value) -> Value {
        serde_json::json!({
            "service": self.name,
            "version": self.version,
            "pid": self.pid,
            "uptime_secs": self.uptime_secs(),
            "protocol_version": agentic_ipc::PROTOCOL_VERSION,
            "details": extra,
        })
    }
}

/// Build a well-formed error envelope.
#[must_use]
pub fn error_envelope(id: String, message: impl Into<String>) -> Envelope {
    Envelope {
        version: agentic_ipc::PROTOCOL_VERSION,
        id,
        payload: Payload::Err {
            message: message.into(),
        },
    }
}

/// Build a well-formed success envelope carrying `data`.
#[must_use]
pub fn ok_envelope(id: String, data: Value) -> Envelope {
    Envelope {
        version: agentic_ipc::PROTOCOL_VERSION,
        id,
        payload: Payload::Ok { data },
    }
}

/// Serve requests for a service until shut down.
///
/// `extra_status` is merged into the standard service status object. The
/// handler is called for every inbound envelope and must never block on a
/// network or an AI provider.
pub async fn run_service(
    config: ServiceConfig,
    extra_status: Value,
    handler: Arc<dyn Fn(&Envelope) -> Envelope + Send + Sync>,
) -> anyhow::Result<()> {
    let context = ServiceContext::for_current_process(&config.name);

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let shutdown_tx = shutdown_tx.clone();

    let status_json = context.status(extra_status);
    let socket = config.socket_path();

    let handler = Arc::clone(&handler);

    let request_handler = Arc::new(move |request: &Envelope| -> Envelope {
        let reply = match &request.payload {
            Payload::Ping => Envelope {
                version: agentic_ipc::PROTOCOL_VERSION,
                id: request.id.clone(),
                payload: Payload::Ok {
                    data: serde_json::json!({ "pong": true }),
                },
            },
            Payload::Status => Envelope {
                version: agentic_ipc::PROTOCOL_VERSION,
                id: request.id.clone(),
                payload: Payload::Ok {
                    data: status_json.clone(),
                },
            },
            Payload::Shutdown => {
                let _ = shutdown_tx.send(true);
                Envelope {
                    version: agentic_ipc::PROTOCOL_VERSION,
                    id: request.id.clone(),
                    payload: Payload::Ok {
                        data: serde_json::json!({ "shutdown": true }),
                    },
                }
            }
            Payload::Command { name, args } => handler(&Envelope {
                version: agentic_ipc::PROTOCOL_VERSION,
                id: request.id.clone(),
                payload: Payload::Command {
                    name: name.clone(),
                    args: args.clone(),
                },
            }),
            other => error_envelope(
                request.id.clone(),
                format!("request type not handled by core runner: {other:?}"),
            ),
        };
        reply
    });

    let serve_fut = agentic_ipc::serve(&socket, request_handler)
        .instrument(tracing::info_span!("service", service = %context.name));

    tokio::pin!(serve_fut);

    let outcome: anyhow::Result<()> = tokio::select! {
        result = &mut serve_fut => result.map_err(anyhow::Error::from),
        changed = shutdown_rx.changed() => {
            match changed {
                Ok(()) if *shutdown_rx.borrow() => {
                    tracing::info!("shutdown requested over ipc");
                    Ok(())
                }
                Ok(()) => Ok(()),
                Err(_) => Ok(()),
            }
        }
    };

    let _ = std::fs::remove_file(&socket);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn service_status_is_reported() {
        let dir = tempdir().unwrap();
        let config = ServiceConfig {
            name: "flowd".to_owned(),
            runtime_dir: dir.path().to_path_buf(),
            database_path: None,
            log_format: crate::config::LogFormat::Text,
        };

        let handler = |request: &Envelope| -> Envelope {
            match &request.payload {
                Payload::Command { name, .. } => Envelope {
                    version: agentic_ipc::PROTOCOL_VERSION,
                    id: request.id.clone(),
                    payload: Payload::Ok {
                        data: serde_json::json!({ "command": name }),
                    },
                },
                _ => error_envelope(request.id.clone(), "unexpected request in test handler"),
            }
        };

        let server = tokio::spawn(run_service(
            config,
            serde_json::json!({}),
            Arc::new(handler),
        ));

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let socket = dir.path().join("flowd.sock");

        let status = agentic_ipc::call(&socket, Payload::Status)
            .await
            .expect("status call");
        assert!(matches!(status.payload, Payload::Ok { .. }));

        let cmd = agentic_ipc::call(
            &socket,
            Payload::Command {
                name: "hello".into(),
                args: serde_json::json!({}),
            },
        )
        .await
        .expect("command call");
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("hello"));

        let shut = agentic_ipc::call(&socket, Payload::Shutdown)
            .await
            .expect("shutdown");
        assert!(matches!(shut.payload, Payload::Ok { .. }));

        server.await.expect("server exits").expect("clean exit");
    }
}
