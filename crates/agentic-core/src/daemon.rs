use std::sync::Arc;
use std::time::Instant;

use agentic_ipc::{Envelope, Payload};
use serde_json::Value;
use tracing::Instrument;

use crate::config::ServiceConfig;

#[derive(Debug, Clone)]
pub struct ServiceContext {
    pub name: String,

    pub version: String,

    pub pid: u32,

    pub started_at: Instant,
}

impl ServiceContext {
    #[must_use]
    pub fn for_current_process(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            pid: std::process::id(),
            started_at: Instant::now(),
        }
    }

    #[must_use]
    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

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

#[must_use]
pub fn ok_envelope(id: String, data: Value) -> Envelope {
    Envelope {
        version: agentic_ipc::PROTOCOL_VERSION,
        id,
        payload: Payload::Ok { data },
    }
}

pub async fn run_service(
    config: ServiceConfig,
    extra_status: Value,
    handler: Arc<dyn Fn(&Envelope) -> Envelope + Send + Sync>,
) -> anyhow::Result<()> {
    run_service_with_status(config, Arc::new(move || extra_status.clone()), handler).await
}

/// Status details are recomputed for each request by the bounded IPC handler,
/// rather than reporting counts captured at service startup.
pub async fn run_service_with_status(
    config: ServiceConfig,
    status_details: Arc<dyn Fn() -> Value + Send + Sync>,
    handler: Arc<dyn Fn(&Envelope) -> Envelope + Send + Sync>,
) -> anyhow::Result<()> {
    let context = ServiceContext::for_current_process(&config.name);

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let shutdown_tx = shutdown_tx.clone();

    let status_context = context.clone();
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
                    data: status_context.status(status_details()),
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

    agentic_ipc::serve_with_shutdown(&socket, request_handler, shutdown_rx)
        .instrument(tracing::info_span!("service", service = %context.name))
        .await
        .map_err(anyhow::Error::from)
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
            state_dir: dir.path().join("state"),
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
        assert!(!socket.exists(), "shutdown must remove the owned socket");
    }
}
