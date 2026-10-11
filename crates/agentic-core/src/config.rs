//! Configuration loading for Flowerly OS services.
//!
//! Configuration is intentionally minimal: a TOML file for static values and
//! environment variables for the few things that vary between runs. Services
//! must be runnable with zero model/network configuration.

use std::env;
use std::fmt;
use std::path::PathBuf;

use serde::Deserialize;
use thiserror::Error;

/// Errors produced during configuration loading.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The configuration file could not be read.
    #[error("cannot read config file `{path}`: {source}")]
    Read {
        /// Path of the offending file.
        path: String,
        /// Underlying error.
        source: std::io::Error,
    },
    /// The TOML file was malformed.
    #[error("invalid config file `{path}`: {message}")]
    Parse {
        /// Path of the offending file.
        path: String,
        /// Toml error message.
        message: String,
    },
}

/// Output format for structured logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Human readable, newline separated (default).
    #[default]
    Text,
    /// JSON lines, suitable for journald/vector pipelines.
    Json,
}

impl fmt::Display for LogFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LogFormat::Text => f.write_str("text"),
            LogFormat::Json => f.write_str("json"),
        }
    }
}

/// Static configuration for a single service.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServiceConfig {
    /// Human-facing service name (e.g. `flowd`).
    pub name: String,
    /// Directory holding the service's Unix sockets and runtime state.
    pub runtime_dir: PathBuf,
    /// Path to the durable SQLite database, when the service persists state.
    pub database_path: Option<PathBuf>,
    /// Log output format.
    pub log_format: LogFormat,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            name: "service".to_owned(),
            runtime_dir: default_runtime_dir(),
            database_path: None,
            log_format: LogFormat::Text,
        }
    }
}

impl ServiceConfig {
    /// Load a config from a TOML file, overriding static values with the
    /// `AGENTIC_RUNTIME_DIR`, `AGENTIC_DATABASE_PATH` and `AGENTIC_LOG_FORMAT`
    /// environment variables when present.
    pub fn load(path: &std::path::Path) -> Result<Self, ConfigError> {
        let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.display().to_string(),
            source,
        })?;
        let mut config: Self = toml::from_str(&raw).map_err(|e| ConfigError::Parse {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        config.apply_environment_overrides();
        Ok(config)
    }

    /// Build a configuration for the named service using defaults, then apply
    /// environment overrides. Equivalent to `load` without a config file.
    #[must_use]
    pub fn for_service(name: &str) -> Self {
        let mut config = Self {
            name: name.to_owned(),
            ..Self::default()
        };
        config.apply_environment_overrides();
        config
    }

    fn apply_environment_overrides(&mut self) {
        if let Ok(dir) = env::var("AGENTIC_RUNTIME_DIR") {
            if !dir.is_empty() {
                self.runtime_dir = PathBuf::from(dir);
            }
        }
        if let Ok(db) = env::var("AGENTIC_DATABASE_PATH") {
            if !db.is_empty() {
                self.database_path = Some(PathBuf::from(db));
            }
        }
        if let Ok(fmt) = env::var("AGENTIC_LOG_FORMAT") {
            self.log_format = match fmt.to_ascii_lowercase().as_str() {
                "json" => LogFormat::Json,
                "text" => LogFormat::Text,
                other => {
                    tracing::warn!(value = other, "unknown AGENTIC_LOG_FORMAT, using text");
                    LogFormat::Text
                }
            };
        }
    }

    /// The full path of this service's IPC socket.
    #[must_use]
    pub fn socket_path(&self) -> PathBuf {
        agentic_ipc::socket_path(&self.runtime_dir, &self.name)
    }
}

/// The location under which Flowerly OS creates runtime sockets and state.
#[must_use]
pub fn default_runtime_dir() -> PathBuf {
    if let Ok(dir) = env::var("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("flowerly");
        }
    }
    std::env::temp_dir().join(format!("flowerly-{}", user_slug()))
}

/// A stable, filesystem-safe slug identifying the current user, derived from
/// the `USER`/`LOGNAME` environment and falling back to a constant.
#[must_use]
fn user_slug() -> String {
    for key in ["LOGNAME", "USER"] {
        if let Ok(name) = env::var(key) {
            let cleaned: String = name
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            if !cleaned.is_empty() {
                return cleaned;
            }
        }
    }
    "default".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let config = ServiceConfig::default();
        assert_eq!(config.name, "service");
        assert_eq!(config.log_format, LogFormat::Text);
        assert!(config.socket_path().ends_with("service.sock"));
    }

    #[test]
    fn toml_can_be_parsed() {
        let toml = r#"
            name = "flowd"
        "#;
        let config: ServiceConfig = toml::from_str(toml).expect("parse minimal toml");
        assert_eq!(config.name, "flowd");
    }
}
