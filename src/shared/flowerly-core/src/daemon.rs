use std::sync::Arc;
use std::time::Instant;

use crate::ipc::{AccessPolicy, AuthenticatedHandler, Envelope, ErrorCode, Payload, PeerIdentity};
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
            "protocol_version": crate::ipc::PROTOCOL_VERSION,
            "details": extra,
        })
    }
}

#[must_use]
pub fn coded_error_envelope(id: String, code: ErrorCode, message: impl Into<String>) -> Envelope {
    Envelope {
        version: crate::ipc::PROTOCOL_VERSION,
        id,
        payload: Payload::Err {
            message: message.into(),
            code: Some(code),
        },
    }
}

#[must_use]
pub fn error_envelope(id: String, message: impl Into<String>) -> Envelope {
    coded_error_envelope(id, ErrorCode::Internal, message)
}

#[must_use]
pub fn ok_envelope(id: String, data: Value) -> Envelope {
    Envelope {
        version: crate::ipc::PROTOCOL_VERSION,
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
    let handler: Arc<AuthenticatedHandler> = Arc::new(move |request, _peer| handler(request));
    serve(
        config,
        AccessPolicy::default(),
        ShutdownAuthority::ServiceAccount,
        status_details,
        handler,
    )
    .await
}

/// Like [`run_service_with_status`], but admits peers by `policy`, hands
/// the kernel-attested peer to `handler` for per-command authorization, and
/// refuses IPC `shutdown` to anyone outside the admin group.
pub async fn run_authorized_service(
    config: ServiceConfig,
    policy: AccessPolicy,
    status_details: Arc<dyn Fn() -> Value + Send + Sync>,
    handler: Arc<AuthenticatedHandler>,
) -> anyhow::Result<()> {
    serve(
        config,
        policy,
        ShutdownAuthority::AdminGroup,
        status_details,
        handler,
    )
    .await
}

/// Who may stop the service over IPC. SIGTERM from the service account or
/// the service manager is unaffected.
#[derive(Clone, Copy)]
enum ShutdownAuthority {
    ServiceAccount,
    AdminGroup,
}

async fn serve(
    config: ServiceConfig,
    policy: AccessPolicy,
    shutdown_authority: ShutdownAuthority,
    status_details: Arc<dyn Fn() -> Value + Send + Sync>,
    handler: Arc<AuthenticatedHandler>,
) -> anyhow::Result<()> {
    let context = ServiceContext::for_current_process(&config.name);

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let shutdown_tx = shutdown_tx.clone();

    let status_context = context.clone();
    let socket = config.socket_path();

    let handler = Arc::clone(&handler);

    let request_handler = Arc::new(move |request: &Envelope, peer: &PeerIdentity| -> Envelope {
        let reply = match &request.payload {
            Payload::Ping => Envelope {
                version: crate::ipc::PROTOCOL_VERSION,
                id: request.id.clone(),
                payload: Payload::Ok {
                    data: serde_json::json!({ "pong": true }),
                },
            },
            Payload::Status => Envelope {
                version: crate::ipc::PROTOCOL_VERSION,
                id: request.id.clone(),
                payload: Payload::Ok {
                    data: status_context.status(status_details()),
                },
            },
            Payload::Shutdown
                if matches!(shutdown_authority, ShutdownAuthority::AdminGroup)
                    && !policy.is_admin(peer) =>
            {
                coded_error_envelope(
                    request.id.clone(),
                    ErrorCode::PermissionDenied,
                    "shutdown requires membership of the configured admin group",
                )
            }
            Payload::Shutdown => {
                let _ = shutdown_tx.send(true);
                Envelope {
                    version: crate::ipc::PROTOCOL_VERSION,
                    id: request.id.clone(),
                    payload: Payload::Ok {
                        data: serde_json::json!({ "shutdown": true }),
                    },
                }
            }
            Payload::Command { name, args } => {
                // The connected client's asserted created_by is a label, not
                // authority. Use the UID supplied by the Unix kernel for
                // durable creator fields and CreatedBy provenance.
                let mut trusted_args = args.clone();
                let Value::Object(fields) = &mut trusted_args else {
                    return coded_error_envelope(
                        request.id.clone(),
                        ErrorCode::InvalidArgument,
                        "command args must be a JSON object",
                    );
                };
                fields.insert(
                    "created_by".to_owned(),
                    Value::String(format!("uid:{}", peer.uid)),
                );
                handler(
                    &Envelope {
                        version: crate::ipc::PROTOCOL_VERSION,
                        id: request.id.clone(),
                        payload: Payload::Command {
                            name: name.clone(),
                            args: trusted_args,
                        },
                    },
                    peer,
                )
            }
            other => coded_error_envelope(
                request.id.clone(),
                ErrorCode::InvalidArgument,
                format!("request type not handled by core runner: {other:?}"),
            ),
        };
        reply
    });

    crate::ipc::serve_with_policy(&socket, policy, request_handler, shutdown_rx)
        .instrument(tracing::info_span!("service", service = %context.name))
        .await
        .map_err(anyhow::Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn creator_identity_is_attested_not_client_controlled() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempdir().unwrap();
        let config = ServiceConfig {
            name: "trusted-actor".to_owned(),
            runtime_dir: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            database_path: None,
            log_format: crate::config::LogFormat::Text,
            ..ServiceConfig::default()
        };
        let server = tokio::spawn(run_service(
            config,
            serde_json::json!({}),
            Arc::new(|request| match &request.payload {
                Payload::Command { args, .. } => ok_envelope(request.id.clone(), args.clone()),
                _ => error_envelope(request.id.clone(), "unexpected request"),
            }),
        ));

        let socket = dir.path().join("trusted-actor.sock");
        for _ in 0..50 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let expected_actor = format!("uid:{}", std::fs::metadata(&socket).unwrap().uid());
        for args in [
            serde_json::json!({"created_by": "forged-admin", "objective": "test"}),
            serde_json::json!({"objective": "test"}),
        ] {
            let response = crate::ipc::call(
                &socket,
                Payload::Command {
                    name: "goal.create".to_owned(),
                    args,
                },
            )
            .await
            .unwrap();
            match response.payload {
                Payload::Ok { data } => {
                    assert_eq!(data["created_by"], expected_actor);
                }
                _ => panic!("expected echoed command arguments"),
            }
        }
        let invalid = crate::ipc::call(
            &socket,
            Payload::Command {
                name: "goal.create".to_owned(),
                args: serde_json::json!(["not", "an", "object"]),
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            invalid.payload,
            Payload::Err {
                code: Some(ErrorCode::InvalidArgument),
                ..
            }
        ));
        crate::ipc::call(&socket, Payload::Shutdown).await.unwrap();
        server.await.unwrap().unwrap();
    }

    async fn wait_for(socket: &std::path::Path) {
        for _ in 0..100 {
            if crate::ipc::call(socket, Payload::Ping).await.is_ok() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("service never answered on {}", socket.display());
    }

    fn own_primary_gid(dir: &std::path::Path) -> u32 {
        use std::os::unix::fs::MetadataExt;
        let probe = dir.join("gid-probe");
        std::fs::write(&probe, "").unwrap();
        std::fs::metadata(&probe).unwrap().gid()
    }

    /// #31: IPC shutdown is an Admin operation. Without an admin group the
    /// service account itself is refused, and forged JSON cannot help.
    #[tokio::test]
    async fn authorized_service_refuses_shutdown_outside_the_admin_group() {
        let dir = tempdir().unwrap();
        let config = ServiceConfig {
            name: "guarded".to_owned(),
            runtime_dir: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            ..ServiceConfig::default()
        };
        // A gid no account on a test host plausibly belongs to.
        let foreign_gid = 3_999_999_999;
        let server = tokio::spawn(run_authorized_service(
            config,
            AccessPolicy {
                users_gid: None,
                admin_gid: Some(foreign_gid),
            },
            Arc::new(|| serde_json::json!({})),
            Arc::new(|request: &Envelope, peer: &PeerIdentity| {
                ok_envelope(
                    request.id.clone(),
                    serde_json::json!({
                        "service_account": peer.service_account,
                        "groups": peer.groups.to_vec(),
                    }),
                )
            }),
        ));
        let socket = dir.path().join("guarded.sock");
        wait_for(&socket).await;
        let refused = crate::ipc::call(&socket, Payload::Shutdown).await.unwrap();
        assert!(
            matches!(
                refused.payload,
                Payload::Err {
                    code: Some(ErrorCode::PermissionDenied),
                    ..
                }
            ),
            "{refused:?}"
        );
        // Still serving, and the handler sees the kernel-attested peer.
        let reply = crate::ipc::call(
            &socket,
            Payload::Command {
                name: "x.y".to_owned(),
                args: serde_json::json!({"admin": true, "groups": [foreign_gid]}),
            },
        )
        .await
        .unwrap();
        let Payload::Ok { data } = reply.payload else {
            panic!("expected handler reply");
        };
        assert_eq!(data["service_account"], true);
        assert!(!data["groups"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!(foreign_gid)));
        server.abort();
    }

    #[tokio::test]
    async fn authorized_service_accepts_shutdown_from_an_admin_group_member() {
        let dir = tempdir().unwrap();
        let config = ServiceConfig {
            name: "admin-stop".to_owned(),
            runtime_dir: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            ..ServiceConfig::default()
        };
        let gid = own_primary_gid(dir.path());
        let server = tokio::spawn(run_authorized_service(
            config,
            AccessPolicy {
                users_gid: None,
                admin_gid: Some(gid),
            },
            Arc::new(|| serde_json::json!({})),
            Arc::new(|request: &Envelope, _peer: &PeerIdentity| {
                ok_envelope(request.id.clone(), serde_json::json!({}))
            }),
        ));
        let socket = dir.path().join("admin-stop.sock");
        wait_for(&socket).await;
        let stopped = crate::ipc::call(&socket, Payload::Shutdown).await.unwrap();
        assert!(matches!(stopped.payload, Payload::Ok { .. }), "{stopped:?}");
        server.await.unwrap().unwrap();
        assert!(!socket.exists());
    }

    #[tokio::test]
    async fn status_provider_is_evaluated_per_request() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = tempdir().unwrap();
        let config = ServiceConfig {
            name: "dynamic-status".to_owned(),
            runtime_dir: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            database_path: None,
            log_format: crate::config::LogFormat::Text,
            ..ServiceConfig::default()
        };
        let count = Arc::new(AtomicUsize::new(1));
        let status_count = Arc::clone(&count);
        let server = tokio::spawn(run_service_with_status(
            config,
            Arc::new(
                move || serde_json::json!({"item_count": status_count.load(Ordering::SeqCst)}),
            ),
            Arc::new(|req| error_envelope(req.id.clone(), "not implemented")),
        ));
        let socket = dir.path().join("dynamic-status.sock");
        for _ in 0..50 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let first = crate::ipc::call(&socket, Payload::Status).await.unwrap();
        count.store(2, Ordering::SeqCst);
        let second = crate::ipc::call(&socket, Payload::Status).await.unwrap();
        match (first.payload, second.payload) {
            (Payload::Ok { data: before }, Payload::Ok { data: after }) => {
                assert_eq!(before["details"]["item_count"], 1);
                assert_eq!(after["details"]["item_count"], 2);
            }
            _ => panic!("expected status responses"),
        }
        crate::ipc::call(&socket, Payload::Shutdown).await.unwrap();
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn service_status_is_reported() {
        let dir = tempdir().unwrap();
        let config = ServiceConfig {
            name: "flowd".to_owned(),
            runtime_dir: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            database_path: None,
            log_format: crate::config::LogFormat::Text,
            ..ServiceConfig::default()
        };

        let handler = |request: &Envelope| -> Envelope {
            match &request.payload {
                Payload::Command { name, .. } => Envelope {
                    version: crate::ipc::PROTOCOL_VERSION,
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

        let status = crate::ipc::call(&socket, Payload::Status)
            .await
            .expect("status call");
        assert!(matches!(status.payload, Payload::Ok { .. }));

        let cmd = crate::ipc::call(
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

        let shut = crate::ipc::call(&socket, Payload::Shutdown)
            .await
            .expect("shutdown");
        assert!(matches!(shut.payload, Payload::Ok { .. }));

        server.await.expect("server exits").expect("clean exit");
        assert!(!socket.exists(), "shutdown must remove the owned socket");
    }
}
