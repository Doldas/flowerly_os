//! Shared runtime foundations for Flowerly OS system services.
//!
//! `agentic-core` supplies the small, boring pieces every long-lived daemon
//! needs: configuration loading, structured logging, and a health/status
//! server over the local IPC protocol. Nothing here depends on a network or an
//! AI provider.

#![forbid(unsafe_code)]

mod config;
mod daemon;
mod logging;

pub use config::{default_runtime_dir, ConfigError, LogFormat, ServiceConfig};
pub use daemon::{error_envelope, ok_envelope, run_service, ServiceContext};
pub use logging::init_tracing;
