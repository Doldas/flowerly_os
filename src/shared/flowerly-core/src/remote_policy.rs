//! The administrator gate for remote inference providers (ADR-0022 §2).
//!
//! Remote inference is **disabled by default**: it is enabled only by a
//! TOML file the administrator places at a path given on flowerlyd's
//! command line (never from IPC or the environment). The file must not be
//! a symlink, must be owned by root or the service account, and must not
//! be group- or world-writable. Any problem leaves remote inference
//! disabled with a stated reason; local administration is unaffected.
//!
//! The file names each provider's local socket, its destination host and
//! its model allowlist. It cannot hold a credential: unknown fields are
//! refused, and the key lives only in the provider's systemd credential.
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;

/// Owner Q3: a consent grant lasts at most 30 days.
pub const MAX_CONSENT_DAYS: u64 = 30;
/// Owner Q4: the opt-in, memory-only response replay window is short.
pub const MAX_REPLAY_SECONDS: u64 = 900;
const MAX_PROVIDERS: usize = 16;
const MAX_IN_FLIGHT_PER_USER: u32 = 8;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    enabled: bool,
    #[serde(default)]
    response_replay_seconds: u64,
    #[serde(default)]
    provider: Vec<RawProvider>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProvider {
    name: String,
    socket: PathBuf,
    destination: String,
    models: Vec<String>,
    max_in_flight_per_user: Option<u32>,
}

/// One administrator-approved remote provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteProvider {
    /// Matches the `name` of the registered `remote-openai-compatible`
    /// provider row.
    pub name: String,
    /// The provider process's IPC socket (absolute path).
    pub socket: PathBuf,
    /// The host prompts are sent to; consent is bound to it.
    pub destination: String,
    pub models: Vec<String>,
    pub max_in_flight_per_user: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteProviders {
    pub providers: Vec<RemoteProvider>,
    /// 0 (default): generated text is delivered once and not retained.
    pub response_replay_seconds: u64,
}

impl RemoteProviders {
    #[must_use]
    pub fn provider(&self, name: &str) -> Option<&RemoteProvider> {
        self.providers.iter().find(|provider| provider.name == name)
    }

    /// Parse and validate the file's contents.
    ///
    /// # Errors
    /// A description of the first invalid field.
    pub fn parse(text: &str) -> Result<Self, String> {
        let raw: RawFile = toml::from_str(text)
            .map_err(|error| format!("invalid remote provider policy: {error}"))?;
        if !raw.enabled {
            return Err("`enabled` is false".to_owned());
        }
        if raw.response_replay_seconds > MAX_REPLAY_SECONDS {
            return Err(format!(
                "`response_replay_seconds` must be 0..={MAX_REPLAY_SECONDS}"
            ));
        }
        if raw.provider.is_empty() || raw.provider.len() > MAX_PROVIDERS {
            return Err(format!("list 1..={MAX_PROVIDERS} [[provider]] entries"));
        }
        let mut providers = Vec::with_capacity(raw.provider.len());
        for entry in raw.provider {
            if entry.name.is_empty()
                || entry.name.len() > 64
                || !entry
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            {
                return Err("provider `name` must be 1..=64 letters, digits, ., _ or -".to_owned());
            }
            if providers
                .iter()
                .any(|known: &RemoteProvider| known.name == entry.name)
            {
                return Err(format!("provider `{}` is listed twice", entry.name));
            }
            if !entry.socket.is_absolute()
                || entry
                    .socket
                    .components()
                    .any(|part| part == std::path::Component::ParentDir)
                || entry.socket.extension().and_then(|ext| ext.to_str()) != Some("sock")
            {
                return Err(format!(
                    "provider `{}` socket must be an absolute *.sock path without ..",
                    entry.name
                ));
            }
            let destination = entry.destination.to_ascii_lowercase();
            if destination.is_empty()
                || destination.len() > 253
                || !destination
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b".-:".contains(&byte))
            {
                return Err(format!(
                    "provider `{}` destination must be a host name",
                    entry.name
                ));
            }
            if entry.models.is_empty()
                || entry.models.len() > 64
                || entry.models.iter().any(|model| {
                    model.is_empty()
                        || model.len() > 128
                        || !model.bytes().all(|b| b.is_ascii_graphic())
                })
            {
                return Err(format!(
                    "provider `{}` must list 1..=64 model ids",
                    entry.name
                ));
            }
            let max_in_flight_per_user = entry.max_in_flight_per_user.unwrap_or(2);
            if !(1..=MAX_IN_FLIGHT_PER_USER).contains(&max_in_flight_per_user) {
                return Err(format!(
                    "`max_in_flight_per_user` must be 1..={MAX_IN_FLIGHT_PER_USER}"
                ));
            }
            providers.push(RemoteProvider {
                name: entry.name,
                socket: entry.socket,
                destination,
                models: entry.models,
                max_in_flight_per_user,
            });
        }
        Ok(Self {
            providers,
            response_replay_seconds: raw.response_replay_seconds,
        })
    }
}

/// Whether remote inference is available, and if not, why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteGate {
    Disabled { reason: String },
    Enabled(Arc<RemoteProviders>),
}

impl Default for RemoteGate {
    fn default() -> Self {
        Self::Disabled {
            reason: "remote inference is disabled: no administrator policy is configured"
                .to_owned(),
        }
    }
}

impl RemoteGate {
    /// Load the gate from `path`. Never fails: every problem is a
    /// `Disabled` reason for the journal and for `inference.capabilities`.
    #[must_use]
    pub fn load(path: Option<&Path>) -> Self {
        let Some(path) = path else {
            return Self::default();
        };
        let disabled = |reason: String| Self::Disabled {
            reason: format!(
                "remote inference is disabled: {} ({reason})",
                path.display()
            ),
        };
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(error) => return disabled(error.to_string()),
        };
        // The service's effective uid owns /proc/self.
        let service_uid = std::fs::metadata("/proc/self").map(|own| own.uid()).ok();
        if let Err(reason) = check_metadata(&metadata, service_uid) {
            return disabled(reason);
        }
        match std::fs::read_to_string(path)
            .map_err(|error| error.to_string())
            .and_then(|text| RemoteProviders::parse(&text))
        {
            Ok(providers) => Self::Enabled(Arc::new(providers)),
            Err(reason) => disabled(reason),
        }
    }

    #[must_use]
    pub fn providers(&self) -> Option<&RemoteProviders> {
        match self {
            Self::Enabled(providers) => Some(providers),
            Self::Disabled { .. } => None,
        }
    }
}

fn check_metadata(metadata: &std::fs::Metadata, service_uid: Option<u32>) -> Result<(), String> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("not a regular file".to_owned());
    }
    if metadata.uid() != 0 && Some(metadata.uid()) != service_uid {
        return Err("owned by neither root nor the service account".to_owned());
    }
    if metadata.mode() & 0o022 != 0 {
        return Err(format!(
            "mode {:o} is group- or world-writable",
            metadata.mode() & 0o777
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const VALID: &str = "enabled = true\n\
        [[provider]]\n\
        name = \"gamitron\"\n\
        socket = \"/run/flowerly-provider/gamitron.sock\"\n\
        destination = \"OpenUI.GamitronServers.com\"\n\
        models = [\"gpt-oss:20b\"]\n";

    #[test]
    fn a_valid_policy_is_parsed_and_normalized() {
        let policy = RemoteProviders::parse(VALID).unwrap();
        let provider = policy.provider("gamitron").unwrap();
        assert_eq!(provider.destination, "openui.gamitronservers.com");
        assert_eq!(provider.max_in_flight_per_user, 2);
        assert_eq!(
            policy.response_replay_seconds, 0,
            "replay must default to off"
        );
        assert!(policy.provider("other").is_none());
    }

    #[test]
    fn disabled_unknown_and_secret_bearing_policies_are_refused() {
        for (case, text) in [
            (
                "disabled",
                VALID.replace("enabled = true", "enabled = false"),
            ),
            ("key field", format!("{VALID}api_key = \"sk-x\"\n")),
            (
                "credential field",
                VALID.replace("models =", "credential = \"k\"\nmodels ="),
            ),
            (
                "relative socket",
                VALID.replace("/run/flowerly-provider/gamitron.sock", "gamitron.sock"),
            ),
            (
                "parent dir",
                VALID.replace("/run/flowerly-provider/", "/run/../tmp/"),
            ),
            (
                "not a socket",
                VALID.replace("gamitron.sock", "gamitron.txt"),
            ),
            (
                "url destination",
                VALID.replace("OpenUI.GamitronServers.com", "https://x/"),
            ),
            ("no models", VALID.replace("[\"gpt-oss:20b\"]", "[]")),
            (
                "replay too long",
                VALID.replace(
                    "enabled = true",
                    "enabled = true\nresponse_replay_seconds = 901",
                ),
            ),
            ("no providers", "enabled = true\n".to_owned()),
            (
                "duplicate",
                format!("{VALID}{}", &VALID["enabled = true\n".len()..]),
            ),
        ] {
            assert!(RemoteProviders::parse(&text).is_err(), "{case} accepted");
        }
    }

    #[test]
    fn the_deployment_template_parses_with_replay_off() {
        let template =
            include_str!("../../../../distro/templates/remote-inference/remote-providers.toml")
                .replace("<instance>", "gamitron");
        let policy = RemoteProviders::parse(&template).unwrap();
        assert_eq!(policy.response_replay_seconds, 0);
        assert_eq!(
            policy.provider("gamitron").unwrap().socket,
            PathBuf::from("/run/flowerly-provider/gamitron.sock")
        );
    }

    #[test]
    fn the_gate_is_disabled_by_default_and_on_unsafe_files() {
        assert!(matches!(
            RemoteGate::load(None),
            RemoteGate::Disabled { .. }
        ));
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("absent.toml");
        assert!(matches!(
            RemoteGate::load(Some(&absent)),
            RemoteGate::Disabled { .. }
        ));

        let path = dir.path().join("remote.toml");
        std::fs::write(&path, VALID).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(RemoteGate::load(Some(&path)).providers().is_some());

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).unwrap();
        let RemoteGate::Disabled { reason } = RemoteGate::load(Some(&path)) else {
            panic!("group-writable policy enabled remote inference");
        };
        assert!(reason.contains("writable"), "{reason}");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let link = dir.path().join("link.toml");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(matches!(
            RemoteGate::load(Some(&link)),
            RemoteGate::Disabled { .. }
        ));

        // A file owned by neither root nor the service account is refused.
        let metadata = std::fs::metadata(&path).unwrap();
        if metadata.uid() != 0 {
            let refusal = check_metadata(&metadata, Some(metadata.uid() + 1)).unwrap_err();
            // The refusal is logged: it must not disclose the owner's uid.
            assert!(!refusal.contains(&metadata.uid().to_string()));
            assert!(refusal.contains("neither root nor the service account"));
            assert!(check_metadata(&metadata, Some(metadata.uid())).is_ok());
        }
    }
}
