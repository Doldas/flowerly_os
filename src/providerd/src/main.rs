// Flowerly OS reference local inference provider.
//
// Contract claims (RFC 0004 interface 2, tranche 4):
// - Fault-isolated: a separate process so a provider outage cannot corrupt
//   or block the Flowerly runtime.
// - Fail-closed: single-user local Unix socket only (the crate-provided
//   runner enforces that the peer owns the socket). No network, no
//   credentials, no egress.
// - Bounded: prompt, max_tokens, per-actor concurrency and result size are
//   capped; requests carry a client-generated request_id echoed verbatim.
// - Synthetic only: deterministic bounded echo output. This process does
//   NOT claim to run real models and never writes durable state.
mod generate;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use clap::Parser;
use flowerly_core::ipc::{Envelope, ErrorCode, Payload};
use flowerly_core::{
    coded_error_envelope, error_envelope, init_tracing, ok_envelope, run_service, ServiceConfig,
};
use serde_json::Value;

#[derive(Debug, Parser)]
#[command(
    name = "providerd",
    version,
    about = "Flowerly OS reference local inference provider"
)]
struct Cli {
    #[arg(long, env = "AGENTIC_CONFIG")]
    config: Option<PathBuf>,
    #[arg(long)]
    self_test: bool,
}

/// In-flight generation requests per trusted actor label, enforcing the
/// bounded-concurrency contract of the provider boundary.
#[derive(Default)]
struct RateGuard {
    in_flight: HashMap<String, u32>,
}

const MAX_IN_FLIGHT_PER_ACTOR: u32 = 4;

fn too_many_in_flight(request: &Envelope) -> Envelope {
    coded_error_envelope(
        request.id.clone(),
        ErrorCode::Unavailable,
        "provider rate limit reached; retry with bounded backoff",
    )
}

fn lock_held_result(
    guard: &Arc<Mutex<RateGuard>>,
) -> std::result::Result<std::sync::MutexGuard<'_, RateGuard>, String> {
    guard
        .lock()
        .map_err(|err| format!("provider rate guard poisoned: {err}"))
}

fn internal_unlocked(request: &Envelope, message: String) -> Envelope {
    coded_error_envelope(request.id.clone(), ErrorCode::Internal, message)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = match cli.config {
        Some(path) => ServiceConfig::load(&path).context("loading provider config")?,
        None => ServiceConfig::for_service("providerd"),
    };
    init_tracing(&config.name, config.log_format);

    if cli.self_test {
        generate::self_test().map_err(anyhow::Error::msg)?;
        println!("providerd self-test ok: synthetic echo reference provider");
        return Ok(());
    }

    let rate_guard = Arc::new(Mutex::new(RateGuard::default()));
    let handler = move |request: &Envelope| -> Envelope {
        let Payload::Command { name, args } = &request.payload else {
            return error_envelope(request.id.clone(), "unexpected request type");
        };
        // The core runner injects the kernel-verified `created_by`; it is
        // only a rate-scope label — authority stays out of scope here.
        let actor = args
            .get("created_by")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        match name.as_str() {
            "provider.discover" => ok_envelope(
                request.id.clone(),
                serde_json::json!({
                    "provider": "echo",
                    "kind": "local-unix",
                    "synthetic": true,
                    "streaming": false,
                    "executor": "none (bounded synthetic echo)",
                    "max_tokens_per_request": generate::MAX_TOKENS_OUT,
                    "max_prompt_bytes": generate::MAX_PROMPT_BYTES,
                    "max_in_flight_per_actor": MAX_IN_FLIGHT_PER_ACTOR,
                    "rate_limit_breach_code": "unavailable",
                    "promise": "reference-contract-only",
                }),
            ),
            "provider.generate" => {
                let taken = match lock_held_result(&rate_guard) {
                    Ok(mut guard) => {
                        let entry = guard.in_flight.entry(actor.clone()).or_insert(0);
                        if *entry >= MAX_IN_FLIGHT_PER_ACTOR {
                            false
                        } else {
                            *entry += 1;
                            true
                        }
                    }
                    Err(message) => return internal_unlocked(request, message),
                };
                if !taken {
                    return too_many_in_flight(request);
                }
                let outcome = generate::generate(args);
                match lock_held_result(&rate_guard) {
                    Ok(mut guard) => {
                        if let Some(entry) = guard.in_flight.get_mut(&actor) {
                            if *entry <= 1 {
                                guard.in_flight.remove(&actor);
                            } else {
                                *entry -= 1;
                            }
                        }
                    }
                    Err(message) => return internal_unlocked(request, message),
                }
                match outcome {
                    Ok(result) => ok_envelope(request.id.clone(), result),
                    Err(error) => {
                        coded_error_envelope(request.id.clone(), error.code, error.message)
                    }
                }
            }
            other => coded_error_envelope(
                request.id.clone(),
                ErrorCode::InvalidArgument,
                format!("providerd: unknown command `{other}`"),
            ),
        }
    };
    run_service(
        config,
        serde_json::json!({"synthetic": true}),
        Arc::new(handler),
    )
    .await
    .context("providerd terminated")
}
