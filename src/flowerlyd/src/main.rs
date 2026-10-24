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
        Ok(db) => serde_json::json!({
            "ready": true,
            "modules": ["capability","compute","flow","goal","session","generation","provenance","execution"],
            "flow_count": db.list_flows().map(|rows| rows.len()).ok(),
            "default_flow": "General",
            "goal_count": db.list_goals().map(|rows| rows.len()).ok(),
            "session_count": db.list_sessions().map(|rows| rows.len()).ok(),
            "execution_count": db.list_executions().map(|rows| rows.len()).ok(),
            "generation_count": db.list_generations().map(|rows| rows.len()).ok(),
            "model_count": db.list_models().map(|rows| rows.len()).ok(),
            "profile_count": db.list_profiles().map(|rows| rows.len()).ok(),
            "active_workload_count": db
                .list_workloads(None)
                .map(|rows| rows.iter().filter(|row| {
                    row.state == flowerly_core::types::WorkloadState::Queued
                        || row.state == flowerly_core::types::WorkloadState::Admitted
                })
                .count())
                .ok(),
            "provider_count": db.list_providers().map(|rows| rows.len()).ok(),
            "database": "ready"
        }),
        Err(_) => serde_json::json!({"ready": false, "reason": "store busy"}),
    });
    let handler = Arc::new(move |request: &Envelope| router.handle(request));
    run_service_with_status(config, status, handler)
        .await
        .context("flowerlyd terminated")
}
