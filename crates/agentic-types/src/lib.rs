//! Core domain types and strongly typed identifiers for Flowerly OS.
//!
//! The rest of the system passes these types instead of raw strings. Every
//! durable object (Flow, Goal, Session, Generation, Execution, ...) has a
//! prefixed, UUID-backed identifier so that logs, provenance edges and the
//! database all refer to the same stable value.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

mod model;

pub use model::*;

/// Error returned when a typed identifier cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdParseError {
    /// The supplied string did not carry the expected `prefix-` marker.
    #[error("invalid {expected} id: expected prefix `{expected}-`, got `{actual}`")]
    Prefix {
        /// The expected prefix (for example `flow`).
        expected: &'static str,
        /// The offending input.
        actual: String,
    },
}

macro_rules! typed_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Generate a new, unique identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(format!(
                    "{}-{}",
                    $prefix,
                    uuid::Uuid::new_v4().simple()
                ))
            }

            /// Wrap an existing string without validation.
            ///
            /// Prefer [`FromStr`] for untrusted input; this constructor exists
            /// for values that already came from the database or an internal
            /// boundary.
            #[must_use]
            pub fn from_raw(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Borrow the underlying string.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consume the identifier and return the underlying string.
            #[must_use]
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let expected = concat!($prefix, "-");
                if s.starts_with(expected) && s.len() > expected.len() {
                    Ok(Self(s.to_owned()))
                } else {
                    Err(IdParseError::Prefix {
                        expected: $prefix,
                        actual: s.to_owned(),
                    })
                }
            }
        }
    };
}

typed_id!(
    /// Identifies a persistent Flow (a coherent human activity context).
    FlowId,
    "flow"
);
typed_id!(
    /// Identifies a persistent Goal.
    GoalId,
    "goal"
);
typed_id!(
    /// Identifies an Agent instance.
    AgentId,
    "agent"
);
typed_id!(
    /// Identifies a persistent terminal/remote Session.
    SessionId,
    "session"
);
typed_id!(
    /// Identifies a system Generation.
    GenerationId,
    "gen"
);
typed_id!(
    /// Identifies an Experiment.
    ExperimentId,
    "exp"
);
typed_id!(
    /// Identifies a single Execution record.
    ExecutionId,
    "exec"
);
typed_id!(
    /// Identifies an Observation.
    ObservationId,
    "obs"
);
typed_id!(
    /// Identifies a Finding.
    FindingId,
    "finding"
);
typed_id!(
    /// Identifies a Decision.
    DecisionId,
    "decision"
);
typed_id!(
    /// Identifies an Artifact.
    ArtifactId,
    "artifact"
);
typed_id!(
    /// Identifies a Project.
    ProjectId,
    "project"
);

/// Milliseconds since the Unix epoch, stored as a small `Copy` newtype so that
/// domain objects stay free of heavy datetime dependencies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    /// The current wall-clock time.
    #[must_use]
    pub fn now() -> Self {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or_default();
        Self(millis)
    }

    /// Construct from a raw millisecond value.
    #[must_use]
    pub fn from_millis(millis: i64) -> Self {
        Self(millis)
    }

    /// The underlying millisecond value.
    #[must_use]
    pub fn as_millis(&self) -> i64 {
        self.0
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A short, human-facing schema/object tag used for provenance endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObjectRef(String);

impl ObjectRef {
    /// Build an object reference from a raw string (e.g. `flow-flow-...`).
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the reference.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ObjectRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_prefixed() {
        let a = FlowId::new();
        let b = FlowId::new();
        assert_ne!(a, b);
        assert!(a.as_str().starts_with("flow-"));
    }

    #[test]
    fn ids_round_trip_through_strings() {
        let id = GoalId::new();
        let parsed: GoalId = id.to_string().parse().expect("valid goal id");
        assert_eq!(id, parsed);
    }

    #[test]
    fn wrong_prefix_is_rejected() {
        assert!("goal-123".parse::<FlowId>().is_err());
        assert!("flow-".parse::<FlowId>().is_err());
        assert!("nonsense".parse::<SessionId>().is_err());
    }

    #[test]
    fn timestamps_are_monotonic_enough() {
        let a = Timestamp::now();
        let b = Timestamp::now();
        assert!(b.as_millis() >= a.as_millis());
    }
}
