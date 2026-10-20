use super::*;
use flowerly_core::ipc::{Envelope, Payload};
use flowerly_core::storage::Store;
use flowerly_core::ServiceConfig;
use std::sync::{Arc, Mutex};

pub(crate) struct SessionRuntime {
    #[cfg(target_os = "linux")]
    supervisor: Arc<pty::Supervisor>,
    #[cfg(target_os = "linux")]
    reconciler: tokio::task::JoinHandle<()>,
}

impl SessionRuntime {
    pub(crate) fn start(config: &ServiceConfig, store: &Arc<Mutex<Store>>) -> anyhow::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let cgroup_root = prepare_cgroup_root().map_err(|error| error.to_string());
            if let Err(reason) = &cgroup_root {
                tracing::warn!(reason, "PTY delegation unavailable; spawning disabled");
            }
            {
                let db = store
                    .lock()
                    .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
                // Reservations are committed before the guardian is launched.
                // A crash may leave a live cgroup WITHOUT a Session row. Kill
                // only its exact, validated pre-reserved cgroup name; never
                // probe or signal a bare PID vulnerable to reuse.
                for id in db
                    .list_external_spawn_reservations()
                    .context("listing reserved external spawns")?
                {
                    if let Ok(Some(root)) = &cgroup_root {
                        terminate_stale_cgroup(&id, Some(root))?;
                    } else {
                        tracing::warn!(
                            session_id = %id,
                            "external spawn recovery cannot prove cgroup cleanup"
                        );
                    }
                }
                for session in db
                    .list_sessions()
                    .context("listing sessions for PTY recovery")?
                {
                    if session.kind == "local-pty" && session.pid.is_some() {
                        if let Ok(Some(root)) = &cgroup_root {
                            terminate_stale_cgroup(&session.id, Some(root))?;
                        } else {
                            tracing::warn!(session_id = %session.id, "PTY recovery requires cgroup delegation");
                        }
                    }
                    if session.kind == "local-pty"
                        && session.pid.is_some()
                        && session.state == SessionState::Running
                    {
                        db.update_session_state(&session.id, SessionState::Suspended)?;
                        db.update_session_presentation_state(
                            &session.id,
                            SessionPresentationState::Detached,
                        )?;
                    }
                }
            }
            let supervisor = Arc::new(pty::Supervisor::new(
                config.runtime_dir.clone(),
                cgroup_root,
            ));
            let worker_supervisor = Arc::clone(&supervisor);
            let worker_store = Arc::clone(store);
            let reconciler = tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
                loop {
                    interval.tick().await;
                    let supervisor = Arc::clone(&worker_supervisor);
                    let store = Arc::clone(&worker_store);
                    if tokio::task::spawn_blocking(move || supervisor.reconcile(&store))
                        .await
                        .is_err()
                    {
                        tracing::error!("PTY reconciler worker failed");
                    }
                }
            });
            Ok(Self {
                supervisor,
                reconciler,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (config, store);
            Ok(Self {})
        }
    }

    pub(crate) fn handle(&self, store: &Arc<Mutex<Store>>, request: &Envelope) -> Envelope {
        #[cfg(target_os = "linux")]
        if let Payload::Command { name, args } = &request.payload {
            if self.supervisor.routes(name, args) {
                return self.supervisor.handle(store, request);
            }
        }
        super::handle_command(store, request)
    }
}
impl Drop for SessionRuntime {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        self.reconciler.abort();
    }
}
