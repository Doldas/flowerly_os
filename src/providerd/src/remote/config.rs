//! Administrator configuration and credential loading for the remote
//! backend (ADR-0022 §2, §4).
//!
//! Invariants:
//! - The destination is an `https://` origin from a root-owned file; it
//!   never comes from IPC arguments, the environment or model output.
//! - The API key is read only from the systemd credentials directory and is
//!   held in [`ApiKey`], whose `Debug` never prints it.
//! - Limits can only be configured *downward* from the compiled ceilings.
use std::fmt;
use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

/// Compiled ceilings; configuration may only lower them.
pub const MAX_PROMPT_BYTES: usize = 16 * 1024;
pub const MAX_TOKENS_CEILING: u64 = 4096;
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// Keeps a replayable flowerlyd response under the 64 KiB operation limit.
pub const MAX_TEXT_BYTES: usize = 32 * 1024;
pub const MAX_DISCOVERED_MODELS: usize = 1024;
pub const MAX_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
pub const MAX_CONCURRENCY: u32 = 16;
const MAX_KEY_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApiStyle {
    /// `/v1/models` and `/v1/chat/completions`.
    Openai,
    /// Open WebUI's `/api/models` and `/api/chat/completions`.
    OpenWebui,
}

impl ApiStyle {
    pub fn models_path(self) -> &'static str {
        match self {
            Self::Openai => "/v1/models",
            Self::OpenWebui => "/api/models",
        }
    }

    pub fn chat_path(self) -> &'static str {
        match self {
            Self::Openai => "/v1/chat/completions",
            Self::OpenWebui => "/api/chat/completions",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::OpenWebui => "open-webui",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    remote: RawRemote,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRemote {
    api_base: String,
    api_style: ApiStyle,
    models: Vec<String>,
    credential: String,
    max_tokens_cap: Option<u64>,
    connect_timeout_ms: Option<u64>,
    request_timeout_ms: Option<u64>,
    max_concurrency: Option<u32>,
    #[serde(default)]
    allow_private_addresses: bool,
}

/// Validated remote backend configuration.
#[derive(Debug, Clone)]
pub struct RemoteConfig {
    /// `https://host[:port]` with no path, userinfo, query or fragment.
    pub origin: String,
    pub host: String,
    pub api_style: ApiStyle,
    pub models: Vec<String>,
    pub credential: String,
    pub max_tokens_cap: u64,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub max_concurrency: u32,
    pub allow_private_addresses: bool,
}

impl RemoteConfig {
    /// Parse and validate the administrator's TOML configuration.
    pub fn parse(text: &str) -> Result<Self, String> {
        let raw: RawFile =
            toml::from_str(text).map_err(|error| format!("invalid remote config: {error}"))?;
        let raw = raw.remote;
        let (origin, host) = parse_origin(&raw.api_base)?;
        if raw.models.is_empty() || raw.models.len() > 64 {
            return Err("`models` must list 1..=64 allowlisted model ids".to_owned());
        }
        for model in &raw.models {
            if !valid_model_id(model) {
                return Err(format!(
                    "model id `{model}` must be 1..=128 visible ASCII characters"
                ));
            }
        }
        if !valid_credential_name(&raw.credential) {
            return Err(
                "`credential` must be a bare credential name (letters, digits, ., _, -)".to_owned(),
            );
        }
        let max_tokens_cap = bounded(
            raw.max_tokens_cap,
            1,
            MAX_TOKENS_CEILING,
            1024,
            "max_tokens_cap",
        )?;
        let connect_timeout = Duration::from_millis(bounded(
            raw.connect_timeout_ms,
            100,
            MAX_CONNECT_TIMEOUT.as_millis() as u64,
            MAX_CONNECT_TIMEOUT.as_millis() as u64,
            "connect_timeout_ms",
        )?);
        let request_timeout = Duration::from_millis(bounded(
            raw.request_timeout_ms,
            100,
            MAX_REQUEST_TIMEOUT.as_millis() as u64,
            60_000,
            "request_timeout_ms",
        )?);
        let max_concurrency = bounded(
            raw.max_concurrency.map(u64::from),
            1,
            u64::from(MAX_CONCURRENCY),
            2,
            "max_concurrency",
        )? as u32;
        Ok(Self {
            origin,
            host,
            api_style: raw.api_style,
            models: raw.models,
            credential: raw.credential,
            max_tokens_cap,
            connect_timeout,
            request_timeout,
            max_concurrency,
            allow_private_addresses: raw.allow_private_addresses,
        })
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("reading remote config {}: {error}", path.display()))?;
        Self::parse(&text)
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }
}

fn bounded(
    value: Option<u64>,
    min: u64,
    max: u64,
    default: u64,
    field: &str,
) -> Result<u64, String> {
    match value {
        None => Ok(default.min(max)),
        Some(raw) if (min..=max).contains(&raw) => Ok(raw),
        Some(_) => Err(format!("`{field}` must be {min}..={max}")),
    }
}

fn valid_model_id(raw: &str) -> bool {
    !raw.is_empty() && raw.len() <= 128 && raw.bytes().all(|byte| byte.is_ascii_graphic())
}

fn valid_credential_name(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 64
        && raw != "."
        && raw != ".."
        && raw
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

/// Accept only `https://host[:port][/]`. Returns the normalized origin and
/// the host (without brackets for IPv6 literals).
fn parse_origin(raw: &str) -> Result<(String, String), String> {
    let rest = raw
        .strip_prefix("https://")
        .ok_or_else(|| "`api_base` must be an https:// origin".to_owned())?;
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.is_empty()
        || authority.contains(['/', '?', '#', '@', '\\', ' '])
        || !authority.is_ascii()
        || authority.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(
            "`api_base` must be https://host[:port] without path, userinfo, query or fragment"
                .to_owned(),
        );
    }
    let (host, port) = if let Some(after) = authority.strip_prefix('[') {
        let (host, tail) = after
            .split_once(']')
            .ok_or_else(|| "`api_base` has an unterminated IPv6 literal".to_owned())?;
        (host, tail.strip_prefix(':'))
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    if let Some(port) = port {
        if port.parse::<u16>().ok().filter(|port| *port > 0).is_none() {
            return Err("`api_base` port must be 1..=65535".to_owned());
        }
    }
    if host.is_empty() {
        return Err("`api_base` has no host".to_owned());
    }
    let host = host.to_ascii_lowercase();
    if host.parse::<IpAddr>().is_err()
        && !host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return Err("`api_base` host is not a valid DNS name or IP literal".to_owned());
    }
    Ok((format!("https://{}", authority.to_ascii_lowercase()), host))
}

/// The provider API key. `Debug`/`Display` never reveal it.
pub struct ApiKey(String);

impl ApiKey {
    /// Validate a key: 1..=4096 bytes of visible ASCII (no whitespace or
    /// control characters, which could split or inject HTTP headers).
    pub fn new(raw: String) -> Result<Self, String> {
        if raw.is_empty()
            || raw.len() > MAX_KEY_BYTES
            || !raw.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(
                "credential must be 1..=4096 bytes of visible ASCII (no whitespace)".to_owned(),
            );
        }
        Ok(Self(raw))
    }

    /// Read `$CREDENTIALS_DIRECTORY/<name>` (systemd `LoadCredential=` /
    /// `LoadCredentialEncrypted=`). A single trailing newline is tolerated.
    pub fn from_credentials_dir(name: &str) -> Result<Self, String> {
        let dir = std::env::var_os("CREDENTIALS_DIRECTORY").ok_or_else(|| {
            "CREDENTIALS_DIRECTORY is not set: supply the key with systemd LoadCredential="
                .to_owned()
        })?;
        Self::from_dir(Path::new(&dir), name)
    }

    pub fn from_dir(dir: &Path, name: &str) -> Result<Self, String> {
        if !valid_credential_name(name) {
            return Err("invalid credential name".to_owned());
        }
        let path = dir.join(name);
        let mut raw = std::fs::read_to_string(&path)
            .map_err(|error| format!("reading credential `{name}`: {}", error.kind()))?;
        if raw.ends_with('\n') {
            raw.pop();
        }
        // The read error above names only the kind, never the contents.
        Self::new(raw).map_err(|error| format!("credential `{name}`: {error}"))
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApiKey([redacted])")
    }
}

impl Drop for ApiKey {
    fn drop(&mut self) {
        // Best-effort scrub of this heap copy; not a guarantee against
        // copies made earlier by the allocator or the HTTP stack.
        let mut bytes = std::mem::take(&mut self.0).into_bytes();
        bytes.fill(0);
        std::hint::black_box(&bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(api_base: &str) -> Result<RemoteConfig, String> {
        RemoteConfig::parse(&format!(
            "[remote]\napi_base = \"{api_base}\"\napi_style = \"openai\"\n\
             models = [\"gpt-oss:20b\"]\ncredential = \"remote.key\"\n"
        ))
    }

    #[test]
    fn only_https_origins_are_accepted() {
        let parsed = config("https://OpenUI.Example.com/").unwrap();
        assert_eq!(parsed.origin, "https://openui.example.com");
        assert_eq!(parsed.host, "openui.example.com");
        assert_eq!(
            parsed.url("/v1/models"),
            "https://openui.example.com/v1/models"
        );
        assert_eq!(config("https://[::1]:8443").unwrap().host, "::1");
        for refused in [
            "http://openui.example.com",
            "https://user:pass@openui.example.com",
            "https://openui.example.com/v1",
            "https://openui.example.com?x=1",
            "https://openui.example.com#f",
            "https://openui.example.com:0",
            "https://openui.example.com:99999",
            "https://",
            "https://bad host",
            "https://exa_mple.com",
            "ftp://openui.example.com",
        ] {
            assert!(config(refused).is_err(), "{refused} accepted");
        }
    }

    #[test]
    fn limits_can_only_be_lowered_and_unknown_fields_are_refused() {
        let base = "[remote]\napi_base = \"https://h.example\"\napi_style = \"open-webui\"\n\
                    models = [\"m\"]\ncredential = \"k\"\n";
        let parsed = RemoteConfig::parse(base).unwrap();
        assert_eq!(parsed.api_style.chat_path(), "/api/chat/completions");
        assert_eq!(parsed.max_tokens_cap, 1024);
        assert_eq!(parsed.max_concurrency, 2);
        assert!(!parsed.allow_private_addresses);
        for extra in [
            "max_tokens_cap = 5000\n",
            "request_timeout_ms = 999999\n",
            "connect_timeout_ms = 6000\n",
            "max_concurrency = 0\n",
            "verify_tls = false\n",
            "proxy = \"http://p\"\n",
        ] {
            assert!(
                RemoteConfig::parse(&format!("{base}{extra}")).is_err(),
                "{extra} accepted"
            );
        }
        for credential in ["../k", "a/b", "", ".."] {
            let text = base.replace(
                "credential = \"k\"",
                &format!("credential = \"{credential}\""),
            );
            assert!(RemoteConfig::parse(&text).is_err(), "{credential} accepted");
        }
    }

    #[test]
    fn keys_are_validated_and_never_printed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ok.key"), "sk-sentinel-123\n").unwrap();
        let key = ApiKey::from_dir(dir.path(), "ok.key").unwrap();
        assert_eq!(key.expose(), "sk-sentinel-123");
        assert_eq!(format!("{key:?}"), "ApiKey([redacted])");
        for bad in ["", "two words", "line\nbreak", "tab\tkey", "x\r"] {
            assert!(ApiKey::new(bad.to_owned()).is_err(), "{bad:?} accepted");
        }
        let missing = ApiKey::from_dir(dir.path(), "absent.key").unwrap_err();
        assert!(missing.contains("not found"), "{missing}");
        std::fs::write(dir.path().join("bad.key"), "sk-sentinel-123 \n").unwrap();
        let invalid = ApiKey::from_dir(dir.path(), "bad.key").unwrap_err();
        assert!(!invalid.contains("sentinel"), "{invalid}");
        assert!(ApiKey::from_dir(dir.path(), "../ok.key").is_err());
    }
}
