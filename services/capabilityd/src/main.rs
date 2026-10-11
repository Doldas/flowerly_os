//! `capabilityd` — identity, authority and policy.
//!
//! At this milestone capabilityd exposes the initial capability catalog and a
//! policy-evaluation stub. Agent identity and delegation arrive with the
//! bounded agent runtime (Sprint 7).

use std::path::PathBuf;
use std::sync::Arc;

use agentic_core::{init_tracing, ok_envelope, run_service, ServiceConfig};
use agentic_ipc::{Envelope, Payload};
use anyhow::Context;
use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "capabilityd",
    version,
    about = "Flowerly OS capability service"
)]
struct Cli {
    /// Path to a TOML configuration file.
    #[arg(long, env = "AGENTIC_CONFIG")]
    config: Option<PathBuf>,
    /// Run a bounded self-test and exit.
    #[arg(long)]
    self_test: bool,
}

/// The initial catalog of capabilities agents may be granted.
const CAPABILITIES: &[&str] = &[
    "file.read",
    "file.write_workspace",
    "process.run",
    "system.query_state",
    "goal.query",
    "goal.create_child",
    "session.query",
    "provenance.query",
    "generation.create_candidate",
    "evolution.run_experiment",
];

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = match cli.config {
        Some(path) => ServiceConfig::load(&path).context("loading config")?,
        None => ServiceConfig::for_service("capabilityd"),
    };
    init_tracing(&config.name, config.log_format);

    if cli.self_test {
        println!(
            "capabilityd self-test ok: {} capabilities",
            CAPABILITIES.len()
        );
        return Ok(());
    }

    let extra = serde_json::json!({ "capabilities": CAPABILITIES.len() });
    let handler = |request: &Envelope| -> Envelope {
        match &request.payload {
            Payload::Command { name, .. } if name == "capability.list" => ok_envelope(
                request.id.clone(),
                serde_json::json!({ "capabilities": CAPABILITIES }),
            ),
            Payload::Command { name, .. } => agentic_core::error_envelope(
                request.id.clone(),
                format!("capabilityd: unknown command `{name}`"),
            ),
            other => agentic_core::error_envelope(
                request.id.clone(),
                format!("capabilityd: unexpected request {other:?}"),
            ),
        }
    };

    run_service(config, extra, Arc::new(handler))
        .await
        .context("capabilityd terminated abnormally")
}
