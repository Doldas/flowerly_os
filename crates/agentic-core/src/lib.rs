#![forbid(unsafe_code)]

mod config;
mod daemon;
mod logging;

pub use config::{default_runtime_dir, ConfigError, LogFormat, ServiceConfig};
pub use daemon::{
    coded_error_envelope, error_envelope, ok_envelope, run_service, run_service_with_status,
    ServiceContext,
};
pub use logging::init_tracing;
