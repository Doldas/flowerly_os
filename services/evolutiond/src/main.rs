use std::path::PathBuf;
use std::sync::Arc;

use agentic_core::{error_envelope, init_tracing, ok_envelope, run_service, ServiceConfig};
use agentic_ipc::{Envelope, Payload};
use anyhow::Context;
use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "evolutiond", version, about = "Flowerly OS evolution service")]
struct Cli {
    #[arg(long, env = "AGENTIC_CONFIG")]
    config: Option<PathBuf>,

    #[arg(long)]
    self_test: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = match cli.config {
        Some(path) => ServiceConfig::load(&path).context("loading config")?,
        None => ServiceConfig::for_service("evolutiond"),
    };
    init_tracing(&config.name, config.log_format);

    if cli.self_test {
        println!("evolutiond self-test ok: evolution world not yet provisioned");
        return Ok(());
    }

    let extra = serde_json::json!({ "world": "not-yet-provisioned" });
    let handler = |request: &Envelope| -> Envelope {
        match &request.payload {
            Payload::Command { name, .. } if name == "evolution.experiment-status" => {
                ok_envelope(request.id.clone(), serde_json::json!({ "status": "idle" }))
            }
            Payload::Command { name, .. } => error_envelope(
                request.id.clone(),
                format!("evolutiond: unknown command `{name}`"),
            ),
            other => error_envelope(
                request.id.clone(),
                format!("evolutiond: unexpected request {other:?}"),
            ),
        }
    };

    run_service(config, extra, Arc::new(handler))
        .await
        .context("evolutiond terminated abnormally")
}
