//! Structured logging initialisation for Flowerly OS services.
//!
//! Logs are written to stderr in either text or JSON-newline form. When the
//! service runs under systemd, stderr is captured by journald and becomes part
//! of the machine's structured logs without extra plumbing.

use crate::config::LogFormat;
use tracing_subscriber::EnvFilter;

/// Initialise the process-global tracing subscriber.
///
/// The filter comes from the `RUST_LOG` environment variable and defaults to
/// `info`. Calling this more than once is ignored.
pub fn init_tracing(service: &str, format: LogFormat) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    match format {
        LogFormat::Json => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .json()
                .with_target(true)
                .try_init();
        }
        LogFormat::Text => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(true)
                .try_init();
        }
    }

    tracing::info!(
        service,
        rust_version = env!("CARGO_PKG_RUST_VERSION"),
        "structured logging initialised"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracing_can_initialise_twice_safely() {
        init_tracing("test", LogFormat::Text);
        init_tracing("test", LogFormat::Json);
        // Reaching this line without a panic is the assertion.
    }
}
