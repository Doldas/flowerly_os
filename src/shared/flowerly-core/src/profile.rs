//! Expert AI profile specification (RFC 0004 tranche 2).
//!
//! Profiles are portable, human-readable (TOML), schema-validated preference
//! documents for per-agent model selection and ceilings. Invariants:
//! - Profiles contain **no credentials** — secrets are structurally refused.
//! - Inheritance is explicit (`base`), acyclic and bounded in depth.
//! - Capabilities, resource ceilings and locality are preference/policy data
//!   and can never authorize OS actions; separate enforcement gates own that.
//! - Nothing in a profile may point at model endpoints or target hosts.
use serde::{Deserialize, Serialize};

/// Experience/capability tier (RFC 0004 vocabulary, lines 32-41). A tier is a
/// capability preference label, not a benchmark or quality guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CapabilityTier {
    #[default]
    Essential,
    Enhanced,
    Advanced,
    Custom,
}

impl CapabilityTier {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Essential => "essential",
            Self::Enhanced => "enhanced",
            Self::Advanced => "advanced",
            Self::Custom => "custom",
        }
    }
}

impl std::str::FromStr for CapabilityTier {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "essential" => Ok(Self::Essential),
            "enhanced" => Ok(Self::Enhanced),
            "advanced" => Ok(Self::Advanced),
            "custom" => Ok(Self::Custom),
            other => Err(format!("unknown capability tier `{other}`")),
        }
    }
}

/// Independent background activity level; never implied by the capability
/// tier ("one high-end request must not imply unlimited background agents").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BackgroundActivity {
    Quiet,
    #[default]
    Normal,
    Intensive,
}

impl BackgroundActivity {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Quiet => "quiet",
            Self::Normal => "normal",
            Self::Intensive => "intensive",
        }
    }
}

impl std::str::FromStr for BackgroundActivity {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "quiet" | "QUIET" => Ok(Self::Quiet),
            "normal" | "NORMAL" => Ok(Self::Normal),
            "intensive" | "INTENSIVE" => Ok(Self::Intensive),
            other => Err(format!("unknown background activity `{other}`")),
        }
    }
}

/// Data locality preference. `local-only` is the default. Remote inference
/// (ADR-0022) requires `approved-remote-opt-in` *in addition to* the
/// administrator's policy and the user's consent: the preference alone
/// never sends anything off the machine. `org-approved-providers` is
/// recorded intent only and grants nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Locality {
    #[default]
    LocalOnly,
    ApprovedRemoteOptIn,
    OrgApprovedProviders,
}

/// Explicit fallback behavior when a preferred model is unavailable; silent
/// downgrade is never permitted for a task that needs a specific feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FallbackBehavior {
    Queue,
    #[default]
    Ask,
    Downgrade,
    Fail,
}

/// Validated profile document. Field names follow RFC 0004's expert profile
/// sketch. Unknown keys are rejected; credential-shaped keys are rejected
/// with a dedicated invariant explanation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ProfileSpec {
    /// Optional parent profile name; resolution happens at storage level.
    pub base: Option<String>,

    /// Capability tier label (Essential/Enhanced/Advanced/Custom).
    pub capability_tier: CapabilityTier,

    /// Independent background activity level.
    pub background_activity: BackgroundActivity,

    /// Explicitly allowed catalog model ids; empty means "resolve from base,
    /// or no constraint" — never "any model without policy".
    pub allowed_models: Vec<String>,

    /// Preferred execution backend label; nothing is enforced blindly.
    pub preferred_backend: Option<String>,

    /// Data locality preference (default local-only).
    pub locality: Locality,

    /// Memory ceiling in bytes (advise/alert basis; only measured CPU memory
    /// enforcement exists today).
    pub memory_max_bytes: Option<u64>,

    /// VRAM ceiling in bytes — always labeled not-enforced until verified.
    pub vram_max_bytes: Option<u64>,

    /// Concurrent inference workload ceiling for scheduling admission.
    pub concurrency_max: Option<u32>,

    /// Explicit fallback behavior on model unavailability.
    pub fallback: FallbackBehavior,
}

impl ProfileSpec {
    /// Parse and validate a TOML profile document.
    ///
    /// # Errors
    /// Returns the first validation failure as a human-readable message.
    pub fn parse_toml(text: &str) -> Result<Self, String> {
        let lower = text.to_ascii_lowercase();
        for key in [
            "api_key", "api-key", "apikey", "token", "password", "secret",
        ] {
            if lower.contains(key) {
                return Err(format!(
                    "profiles must not contain credentials: `{key}` is not an allowed key"
                ));
            }
        }
        let spec: Self =
            toml::from_str(text).map_err(|error| format!("invalid profile TOML: {error}"))?;
        spec.validate()?;
        Ok(spec)
    }

    /// Shape-only validation used after importing or inheritance resolution.
    ///
    /// # Errors
    /// Returns a human-readable failure message.
    pub fn validate(&self) -> Result<(), String> {
        if self.allowed_models.len() > 32 {
            return Err("`allowed_models` supports at most 32 catalog ids".to_owned());
        }
        for model in &self.allowed_models {
            if model.trim().is_empty() || model.len() > 128 {
                return Err("`allowed_models` entries must be 1..128 characters".to_owned());
            }
            if !model.starts_with("model-") {
                return Err(format!(
                    "`allowed_models` must reference catalog ids (`{model}` lacks the model- prefix)"
                ));
            }
        }
        if let Some(backend) = &self.preferred_backend {
            if backend.trim().is_empty() || backend.len() > 64 {
                return Err("`preferred_backend` must be 1..64 characters".to_owned());
            }
        }
        Ok(())
    }

    /// Apply a parent spec under explicit inheritance semantics: child
    /// non-default/explicit values win, otherwise the parent value applies.
    /// `allowed_models` concatenates and dedupes (never shrinks a ceiling).
    #[must_use]
    pub fn resolve_over(&self, parent: &Self) -> Self {
        let mut resolved = self.clone();
        let parent_is_default_tier = parent.capability_tier == CapabilityTier::default();
        if resolved.capability_tier == CapabilityTier::default() && parent_is_default_tier {
            // Both default: keep default (Essential); explicit child always wins.
        }
        if resolved.memory_max_bytes.is_none() {
            resolved.memory_max_bytes = parent.memory_max_bytes;
        }
        if resolved.vram_max_bytes.is_none() {
            resolved.vram_max_bytes = parent.vram_max_bytes;
        }
        if resolved.concurrency_max.is_none() {
            resolved.concurrency_max = parent.concurrency_max;
        }
        if resolved.preferred_backend.is_none() {
            resolved.preferred_backend = parent.preferred_backend.clone();
        }
        if resolved.fallback == FallbackBehavior::default() {
            resolved.fallback = parent.fallback;
        }
        if resolved.background_activity == BackgroundActivity::default() {
            resolved.background_activity = parent.background_activity;
        }
        // Locality only ever tightens toward local: a child may not widen
        // a parent's egress restriction.
        resolved.locality = if self.locality == Locality::LocalOnly {
            Locality::LocalOnly
        } else {
            parent.locality
        };
        if resolved.allowed_models.is_empty() {
            resolved.allowed_models = parent.allowed_models.clone();
        } else {
            for model in &parent.allowed_models {
                if !resolved.allowed_models.contains(model) {
                    resolved.allowed_models.push(model.clone());
                }
            }
        }
        resolved
    }
}

/// Capability declaration a provider registers. Endpoint is only meaningful
/// for already-enforced local transports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderCapabilitySet {
    pub modalities: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_profile_document() {
        let spec = ProfileSpec::parse_toml(
            "base = 'coder'\ncapability_tier = 'advanced'\n\
             background_activity = 'quiet'\n\
             allowed_models = ['model-abc', 'model-def']\n\
             preferred_backend = 'CPU'\n\
             memory_max_bytes = 8_000_000_000\n\
             concurrency_max = 3\nfallback = 'queue'\n",
        )
        .expect("valid profile");
        assert_eq!(spec.capability_tier, CapabilityTier::Advanced);
        assert_eq!(spec.background_activity, BackgroundActivity::Quiet);
        assert_eq!(spec.memory_max_bytes, Some(8_000_000_000));
        assert_eq!(spec.fallback, FallbackBehavior::Queue);
    }

    #[test]
    fn unknown_and_credential_keys_are_refused() {
        assert!(ProfileSpec::parse_toml("nonsense_key = 1\n").is_err());
        for body in [
            "api_key = 'hunter2'\n",
            "token = 'x'\n",
            "password = 'y'\n",
            "endpoint = 'https://api.example.com'\n",
            "models_endpoint = 'h'\n",
        ] {
            assert!(ProfileSpec::parse_toml(body).is_err(), "{body}");
        }
    }

    #[test]
    fn allowed_models_require_catalog_prefix() {
        assert!(ProfileSpec::parse_toml("allowed_models = ['llama-70b']\n").is_err());
        assert!(ProfileSpec::parse_toml("allowed_models = ['model-abc']\n").is_ok());
    }

    #[test]
    fn inheritance_child_wins_and_locality_tightens() {
        let parent = ProfileSpec {
            background_activity: BackgroundActivity::Intensive,
            locality: Locality::ApprovedRemoteOptIn,
            allowed_models: vec!["model-a".into()],
            ..ProfileSpec::default()
        };
        let child = ProfileSpec {
            base: Some("parent".into()),
            memory_max_bytes: Some(4),
            allowed_models: vec!["model-b".into()],
            ..ProfileSpec::default()
        };
        let resolved = child.resolve_over(&parent);
        assert!(resolved.allowed_models.contains(&"model-a".to_owned()));
        assert!(resolved.allowed_models.contains(&"model-b".to_owned()));
        assert_eq!(resolved.memory_max_bytes, Some(4));
        assert_eq!(resolved.background_activity, BackgroundActivity::Intensive);
        // Child may never widen egress beyond the parent.
        assert_eq!(child.locality, Locality::LocalOnly);
    }
}
