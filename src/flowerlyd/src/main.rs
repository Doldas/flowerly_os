//! Flowerly OS unified supervisor. Only the CLI-facing socket leaves this process.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use flowerly_core::ipc::{Envelope, ErrorCode, Payload};
use flowerly_core::storage::Store;
use flowerly_core::{coded_error_envelope, init_tracing, run_service_with_status, ServiceConfig};
use tokio::sync::mpsc;

#[allow(dead_code)]
mod agentic;

mod authorization;
#[allow(dead_code)]
mod capability;
#[allow(dead_code)]
mod compute;
#[allow(dead_code)]
mod flow;
#[allow(dead_code)]
mod generation;
#[allow(dead_code)]
mod goal;
#[allow(dead_code)]
mod provenance;
#[allow(dead_code)]
mod session;

type SharedStore = Arc<Mutex<Store>>;

#[derive(Debug, Parser)]
#[command(name = "flowerlyd", version, about = "Flowerly OS unified supervisor")]
struct Cli {
    #[arg(long, env = "AGENTIC_CONFIG")]
    config: Option<PathBuf>,
    #[arg(long)]
    state_dir: Option<PathBuf>,
    #[arg(long)]
    runtime_dir: Option<PathBuf>,
    #[arg(long)]
    self_test: bool,
}

struct Job {
    request: Envelope,
    reply: std::sync::mpsc::SyncSender<Envelope>,
}
type Queue = mpsc::Sender<Job>;

fn spawn_worker(store: &SharedStore, handler: fn(&SharedStore, &Envelope) -> Envelope) -> Queue {
    let (tx, mut rx) = mpsc::channel::<Job>(64);
    let store = Arc::clone(store);
    tokio::spawn(async move {
        while let Some(job) = rx.recv().await {
            let store = Arc::clone(&store);
            let Job { request, reply } = job;
            let result = tokio::task::spawn_blocking(move || handler(&store, &request)).await;
            match result {
                Ok(response) => {
                    let _ = reply.send(response);
                }
                Err(error) => tracing::error!(%error, "module task panicked"),
            }
        }
    });
    tx
}

const MODULE_DEADLINE: Duration = Duration::from_secs(7);

fn via_queue(tx: &Queue, request: &Envelope) -> Envelope {
    let (reply, received) = std::sync::mpsc::sync_channel(1);
    if tx
        .try_send(Job {
            request: request.clone(),
            reply,
        })
        .is_err()
    {
        return coded_error_envelope(
            request.id.clone(),
            ErrorCode::Unavailable,
            "module queue unavailable or full; the request was not executed",
        );
    }
    match received.recv_timeout(MODULE_DEADLINE) {
        Ok(response) => response,
        // The job may already be running and can still commit.
        Err(_) => coded_error_envelope(
            request.id.clone(),
            ErrorCode::Unavailable,
            format!(
                "module handler deadline exceeded after {} ms; the outcome is unknown",
                MODULE_DEADLINE.as_millis()
            ),
        ),
    }
}

struct Router {
    store: SharedStore,
    session: session::SessionRuntime,
    flow: Queue,
    generation: Queue,
    provenance: Queue,
    compute: Queue,
}

impl Router {
    fn handle(&self, request: &Envelope) -> Envelope {
        let Payload::Command { name, .. } = &request.payload else {
            return coded_error_envelope(
                request.id.clone(),
                ErrorCode::InvalidArgument,
                "command required",
            );
        };
        // Deny by default: only inventoried commands reach a handler.
        if authorization::classify(name).is_none() {
            return coded_error_envelope(
                request.id.clone(),
                ErrorCode::InvalidArgument,
                format!("unknown command: {name}"),
            );
        }
        match name.split('.').next() {
            Some("flow") => via_queue(&self.flow, request),
            Some("generation") => via_queue(&self.generation, request),
            Some("provenance") => via_queue(&self.provenance, request),
            Some("goal") => goal::handle_command(&self.store, request),
            Some("session") => self.session.handle(&self.store, request),
            Some("execution") | Some("agent") => agentic::handle_command(&self.store, request),
            Some("capability") => capability::handle_command(request),
            Some("model") | Some("device") | Some("profile") | Some("provider")
            | Some("workload") | Some("usage") | Some("inference") => {
                via_queue(&self.compute, request)
            }
            _ => coded_error_envelope(
                request.id.clone(),
                ErrorCode::InvalidArgument,
                format!("unknown command: {name}"),
            ),
        }
    }
}

const MODULES: [&str; 8] = [
    "capability",
    "compute",
    "flow",
    "goal",
    "session",
    "generation",
    "provenance",
    "execution",
];

/// Status reports ready only when the database answers a consistent count
/// snapshot; a failed read is reported with its typed storage error code
/// instead of `ready: true` with missing counts.
fn status_details(db: &Store) -> serde_json::Value {
    match db.status_counts() {
        Ok(counts) => serde_json::json!({
            "ready": true,
            "modules": MODULES,
            "default_flow": "General",
            "flow_count": counts.flows,
            "goal_count": counts.goals,
            "session_count": counts.sessions,
            "execution_count": counts.executions,
            "generation_count": counts.generations,
            "provenance_edge_count": counts.provenance_edges,
            "model_count": counts.models,
            "profile_count": counts.profiles,
            "active_workload_count": counts.active_workloads,
            "provider_count": counts.providers,
            "admin_authorization": authorization::ADMIN_POLICY,
            "database": "ready"
        }),
        Err(error) => serde_json::json!({
            "ready": false,
            "modules": MODULES,
            "database": "error",
            "error_code": error.ipc_error_code(),
            "reason": error.to_string(),
        }),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.self_test {
        agentic::self_test().await?;
        compute::self_test().await?;
        flow::self_test().await?;
        goal::self_test().await?;
        generation::self_test().await?;
        provenance::self_test().await?;
        session::self_test().await?;
        println!("flowerlyd self-test ok: unified service modules");
        return Ok(());
    }
    let mut config = match cli.config {
        Some(ref path) => ServiceConfig::load(path).context("loading supervisor config")?,
        None => ServiceConfig::for_service("flowerlyd"),
    };
    config.name = "flowerlyd".into();
    if let Some(state_dir) = cli.state_dir {
        config.state_dir = state_dir;
    }
    if let Some(runtime_dir) = cli.runtime_dir {
        config.runtime_dir = runtime_dir;
    }
    init_tracing(&config.name, config.log_format);
    let mut db = Store::open(config.database_path()).context("opening unified state")?;
    db.migrate().context("migrating unified SQLite state")?;
    db.health_check().context("checking unified state")?;
    db.ensure_general_flow().context("ensuring General Flow")?;
    compute::init_runtime_dir(config.runtime_dir.clone());
    let store: SharedStore = Arc::new(Mutex::new(db));
    let session = session::SessionRuntime::start(&config, &store)?;
    let router = Arc::new(Router {
        flow: spawn_worker(&store, flow::handle_command),
        generation: spawn_worker(&store, generation::handle_command),
        provenance: spawn_worker(&store, provenance::handle_command),
        compute: spawn_worker(&store, compute::handle_command),
        store: Arc::clone(&store),
        session,
    });
    let status_store = Arc::clone(&store);
    let status = Arc::new(move || match status_store.try_lock() {
        Ok(db) => status_details(&db),
        Err(_) => serde_json::json!({"ready": false, "reason": "store busy"}),
    });
    let handler = Arc::new(move |request: &Envelope| router.handle(request));
    run_service_with_status(config, status, handler)
        .await
        .context("flowerlyd terminated")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_is_ready_with_live_counts() {
        let mut store = Store::open_in_memory().expect("open store");
        store.migrate().expect("migrate");
        store.ensure_general_flow().expect("general flow");
        let details = status_details(&store);
        assert_eq!(details["ready"], true, "{details}");
        assert_eq!(details["database"], "ready");
        assert_eq!(details["flow_count"], 1);
        // Bootstrapping General records its created-by provenance edge.
        assert_eq!(details["provenance_edge_count"], 1);
    }

    #[test]
    fn status_is_not_ready_when_the_database_cannot_answer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.db");
        let mut store = Store::open(&path).expect("open store");
        store.migrate().expect("migrate");
        rusqlite::Connection::open(&path)
            .expect("open second connection")
            .execute_batch("DROP TABLE sessions;")
            .expect("damage schema");
        let details = status_details(&store);
        assert_eq!(details["ready"], false, "{details}");
        assert_eq!(details["database"], "error");
        assert_eq!(details["error_code"], "internal");
        assert!(details.get("flow_count").is_none(), "{details}");
    }
}

#[cfg(test)]
mod malformed_arguments {
    use super::*;

    /// Gate B (#31) / #53: client-caused failures must carry a typed code.
    /// Every store-backed command is sent missing, wrongly typed, malformed
    /// and well-formed-but-absent arguments; none may answer `internal` or
    /// an untyped legacy error. PTY commands need a live session runtime
    /// and are covered by the PTY black-box suites.
    #[test]
    fn client_errors_are_never_internal_or_untyped() {
        let mut db = Store::open_in_memory().expect("open store");
        db.migrate().expect("migrate");
        db.ensure_general_flow().expect("general flow");
        let store: SharedStore = Arc::new(Mutex::new(db));
        let commands = [
            "flow.create",
            "flow.list",
            "flow.activate",
            "flow.archive",
            "goal.create",
            "goal.child",
            "goal.show",
            "goal.list",
            "goal.complete",
            "goal.abandon",
            "session.create",
            "session.list",
            "session.terminate",
            "execution.create",
            "execution.show",
            "execution.list",
            "generation.create",
            "generation.list",
            "generation.inspect",
            "generation.activate",
            "provenance.add",
            "provenance.from",
            "provenance.to",
            "model.register",
            "model.list",
            "model.show",
            "model.retire",
            "model.compatibility",
            "device.budget",
            "profile.clone",
            "profile.edit",
            "profile.export",
            "profile.import",
            "profile.list",
            "profile.rename",
            "profile.resolve",
            "profile.show",
            "profile.version_history",
            "provider.list",
            "provider.register",
            "provider.show",
            "workload.submit",
            "workload.list",
            "workload.show",
            "workload.complete",
            "workload.usage_brief",
            "usage.record",
        ];
        let shapes = [
            serde_json::json!({}),
            serde_json::json!({
                "id": 42, "name": 42, "objective": 7, "priority": "x", "from": 1, "to": true,
                "ref": [], "flow_id": 3, "goal_id": 3, "agent_id": 3, "operation_id": 5,
                "parent": 9, "model_id": 1, "profile_id": 1, "profile": 1, "source": 1,
                "version": "x", "workload_id": 1
            }),
            serde_json::json!({
                "id": "bogus", "name": "bogus", "objective": "o", "from": "bogus", "to": "bogus",
                "ref": "bogus", "relation": "bogus", "flow_id": "bogus", "goal_id": "bogus",
                "agent_id": "bogus", "parent": "bogus", "model_id": "bogus", "profile": "bogus",
                "source": "bogus", "workload_id": "bogus"
            }),
            serde_json::json!({
                "id": "flow-00000000000000000000000000000000",
                "ref": "flow:flow-00000000000000000000000000000000",
                "goal_id": "goal-00000000000000000000000000000000",
                "model_id": "model-00000000000000000000000000000000",
                "workload_id": "wl-00000000000000000000000000000000"
            }),
        ];
        let mut answered = 0;
        let mut leaks = Vec::new();
        for command in commands {
            for shape in &shapes {
                let mut args = shape.clone();
                args["created_by"] = serde_json::json!("uid:1000");
                let request = Envelope::new(Payload::Command {
                    name: command.into(),
                    args: args.clone(),
                });
                let reply = match command.split('.').next() {
                    Some("flow") => flow::handle_command(&store, &request),
                    Some("goal") => goal::handle_command(&store, &request),
                    Some("session") => session::handle_command(&store, &request),
                    Some("execution") => agentic::handle_command(&store, &request),
                    Some("generation") => generation::handle_command(&store, &request),
                    Some("provenance") => provenance::handle_command(&store, &request),
                    _ => compute::handle_command(&store, &request),
                };
                answered += 1;
                if let Payload::Err { code, message } = &reply.payload {
                    if matches!(code, None | Some(ErrorCode::Internal)) {
                        leaks.push(format!("{command} {args} -> {code:?}: {message}"));
                    }
                }
            }
        }
        assert_eq!(answered, commands.len() * shapes.len());
        assert!(
            leaks.is_empty(),
            "untyped client errors:\n{}",
            leaks.join("\n")
        );
    }
}
