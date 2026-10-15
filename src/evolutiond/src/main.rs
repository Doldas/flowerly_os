#[allow(dead_code)]
mod kvm;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use flowerly_core::ipc::{Envelope, ErrorCode, Payload};
use flowerly_core::{coded_error_envelope, init_tracing, ok_envelope, run_service, ServiceConfig};

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

    let extra = serde_json::json!({ "world": "not-yet-provisioned", "provisioned": kvm::PROVISIONED });
    let handler = |request: &Envelope| -> Envelope {
        match &request.payload {
            Payload::Command { name, .. } if name == "evolution.experiment-status" => {
                ok_envelope(
                    request.id.clone(),
                    serde_json::json!({
                        "status": "unavailable",
                        "provisioned": kvm::PROVISIONED,
                        "reason": "Evolution World KVM backend is not provisioned"
                    }),
                )
            }
            Payload::Command { name, .. } if name == "evolution.run_experiment" => {
                coded_error_envelope(
                    request.id.clone(),
                    ErrorCode::Unavailable,
                    "Evolution World KVM backend is not provisioned",
                )
            }
            Payload::Command { name, .. } => coded_error_envelope(
                request.id.clone(),
                ErrorCode::InvalidArgument,
                format!("evolutiond: unsupported command `{name}`"),
            ),
            other => coded_error_envelope(
                request.id.clone(),
                ErrorCode::InvalidArgument,
                format!("evolutiond: unexpected request {other:?}"),
            ),
        }
    };

    run_service(config, extra, Arc::new(handler))
        .await
        .context("evolutiond terminated abnormally")
}
