use std::env;
use std::fmt;
use std::path::PathBuf;

use serde::Deserialize;
use thiserror::Error;

use crate::ipc::AccessPolicy;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read config file `{path}`: {source}")]
    Read {
        path: String,

        source: std::io::Error,
    },

    #[error("invalid config file `{path}`: {message}")]
    Parse { path: String, message: String },

    #[error("cannot resolve {role} group `{spec}`: {message}")]
    Group {
        role: &'static str,
        spec: String,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Text,

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

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServiceConfig {
    pub name: String,

    /// Ephemeral runtime state such as Unix sockets and pid-related files.
    pub runtime_dir: PathBuf,

    /// Durable machine/user state. All local Flowerly services share this
    /// location by default while retaining logical ownership of their tables.
    pub state_dir: PathBuf,

    /// Explicit database override, primarily for tests and recovery tooling.
    pub database_path: Option<PathBuf>,

    pub log_format: LogFormat,

    /// Linux group (name or numeric gid) whose members, besides the service
    /// account itself, may connect and run User commands. Unset keeps the
    /// socket private to the service account (mode 0600).
    pub users_group: Option<String>,

    /// Linux group whose members may run Admin commands. Membership is
    /// required even for the service account; unset refuses every Admin
    /// command.
    pub admin_group: Option<String>,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            name: "service".to_owned(),
            runtime_dir: default_runtime_dir(),
            state_dir: default_state_dir(),
            database_path: None,
            log_format: LogFormat::Text,
            users_group: None,
            admin_group: None,
        }
    }
}

impl ServiceConfig {
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
        if let Ok(dir) = env::var("AGENTIC_STATE_DIR") {
            if !dir.is_empty() {
                self.state_dir = PathBuf::from(dir);
            }
        }
        if let Ok(db) = env::var("AGENTIC_DATABASE_PATH") {
            if !db.is_empty() {
                self.database_path = Some(PathBuf::from(db));
            }
        }
        if let Ok(group) = env::var("AGENTIC_USERS_GROUP") {
            if !group.is_empty() {
                self.users_group = Some(group);
            }
        }
        if let Ok(group) = env::var("AGENTIC_ADMIN_GROUP") {
            if !group.is_empty() {
                self.admin_group = Some(group);
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

    /// Resolve the configured groups against `/etc/group`. An unknown or
    /// ambiguous group is an error, so a typo can never silently widen or
    /// drop authorization.
    pub fn access_policy(&self) -> Result<AccessPolicy, ConfigError> {
        let needs_lookup = [&self.users_group, &self.admin_group]
            .into_iter()
            .flatten()
            .any(|spec| spec.parse::<u32>().is_err());
        let groups = if needs_lookup {
            std::fs::read_to_string("/etc/group").map_err(|error| ConfigError::Group {
                role: "configured",
                spec: "/etc/group".to_owned(),
                message: error.to_string(),
            })?
        } else {
            String::new()
        };
        access_policy_from(
            self.users_group.as_deref(),
            self.admin_group.as_deref(),
            &groups,
        )
    }

    #[must_use]
    pub fn socket_path(&self) -> PathBuf {
        crate::ipc::socket_path(&self.runtime_dir, &self.name)
    }

    #[must_use]
    pub fn database_path(&self) -> PathBuf {
        self.database_path
            .clone()
            .unwrap_or_else(|| self.state_dir.join("state.db"))
    }
}

fn access_policy_from(
    users: Option<&str>,
    admin: Option<&str>,
    group_file: &str,
) -> Result<AccessPolicy, ConfigError> {
    let users_gid = users
        .map(|spec| resolve_group("users", spec, group_file))
        .transpose()?;
    let admin_gid = admin
        .map(|spec| resolve_group("admin", spec, group_file))
        .transpose()?;
    if let (Some(users_gid), Some(admin_gid)) = (users_gid, admin_gid) {
        if users_gid == admin_gid {
            return Err(ConfigError::Group {
                role: "admin",
                spec: admin.unwrap_or_default().to_owned(),
                message: "the admin group must differ from the users group".to_owned(),
            });
        }
    }
    Ok(AccessPolicy {
        users_gid,
        admin_gid,
    })
}

/// A numeric gid, or a name with exactly one `/etc/group` entry.
fn resolve_group(role: &'static str, spec: &str, group_file: &str) -> Result<u32, ConfigError> {
    let error = |message: &str| ConfigError::Group {
        role,
        spec: spec.to_owned(),
        message: message.to_owned(),
    };
    if spec.bytes().all(|byte| byte.is_ascii_digit()) {
        return match spec.parse::<u32>() {
            // (gid_t)-1 means "no change" to chown and is never a real group.
            Ok(u32::MAX) | Err(_) => Err(error("not a valid gid")),
            Ok(gid) => Ok(gid),
        };
    }
    let mut matches = group_file.lines().filter_map(|line| {
        let mut fields = line.split(':');
        (fields.next() == Some(spec)).then(|| fields.nth(1).and_then(|gid| gid.parse::<u32>().ok()))
    });
    match (matches.next(), matches.next()) {
        (Some(Some(gid)), None) if gid != u32::MAX => Ok(gid),
        (Some(_), None) => Err(error("malformed /etc/group entry")),
        (Some(_), Some(_)) => Err(error("listed more than once in /etc/group")),
        (None, _) => Err(error(
            "not found in /etc/group; create it or configure a numeric gid",
        )),
    }
}

#[must_use]
pub fn default_runtime_dir() -> PathBuf {
    if let Ok(dir) = env::var("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("flowerly");
        }
    }
    std::env::temp_dir().join(format!("flowerly-{}", user_slug()))
}

#[must_use]
pub fn default_state_dir() -> PathBuf {
    if let Ok(dir) = env::var("XDG_STATE_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("flowerly");
        }
    }

    if let Ok(home) = env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home)
                .join(".local")
                .join("state")
                .join("flowerly");
        }
    }

    std::env::temp_dir().join(format!("flowerly-state-{}", user_slug()))
}

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
        assert!(config.database_path().ends_with("state.db"));
    }

    #[test]
    fn services_share_the_default_database_path() {
        let flowd = ServiceConfig::for_service("flowd");
        let goald = ServiceConfig::for_service("goald");
        assert_eq!(flowd.database_path(), goald.database_path());
    }

    const GROUPS: &str = "root:x:0:\nflowerly-users:x:961:alice,bob\nflowerly-admin:x:962:alice\ndup:x:5:\ndup:x:6:\nbroken:x:\n";

    #[test]
    fn groups_resolve_by_name_or_number() {
        let policy =
            access_policy_from(Some("flowerly-users"), Some("962"), GROUPS).expect("policy");
        assert_eq!(policy.users_gid, Some(961));
        assert_eq!(policy.admin_gid, Some(962));
        assert_eq!(
            access_policy_from(None, None, GROUPS).unwrap(),
            AccessPolicy::default()
        );
    }

    #[test]
    fn unknown_ambiguous_or_invalid_groups_fail_closed() {
        for (users, admin) in [
            (Some("missing"), None),
            (None, Some("missing")),
            (None, Some("dup")),
            (None, Some("broken")),
            (None, Some("4294967295")),
            (None, Some("99999999999")),
            (Some("flowerly-admin"), Some("962")),
        ] {
            assert!(
                access_policy_from(users, admin, GROUPS).is_err(),
                "{users:?} {admin:?}"
            );
        }
    }

    #[test]
    fn groups_are_configurable_in_toml() {
        let config: ServiceConfig =
            toml::from_str("users_group = \"flowerly-users\"\nadmin_group = \"0\"\n")
                .expect("parse");
        assert_eq!(config.users_group.as_deref(), Some("flowerly-users"));
        assert_eq!(config.admin_group.as_deref(), Some("0"));
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
