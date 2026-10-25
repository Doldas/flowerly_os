#![forbid(unsafe_code)]
// One library crate for types, durable storage, framed IPC and daemon runtime.
mod config;
mod daemon;
pub mod ipc;
mod logging;
pub mod profile;
pub mod storage;
pub mod storage_mode;
pub mod types;
pub use config::{default_runtime_dir, ConfigError, LogFormat, ServiceConfig};
pub use daemon::{
    coded_error_envelope, error_envelope, ok_envelope, run_authorized_service, run_service,
    run_service_with_status, ServiceContext,
};
pub use logging::init_tracing;
