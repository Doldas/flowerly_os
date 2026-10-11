#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

mod model;

pub use model::*;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdParseError {
    #[error("invalid {expected} id: expected prefix `{expected}-`, got `{actual}`")]
    Prefix {
        expected: &'static str,

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

            #[must_use]
            pub fn new() -> Self {
                Self(format!(
                    "{}-{}",
                    $prefix,
                    uuid::Uuid::new_v4().simple()
                ))
            }






            #[must_use]
            pub fn from_raw(value: impl Into<String>) -> Self {
                Self(value.into())
            }


            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }


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

typed_id!(FlowId, "flow");
typed_id!(GoalId, "goal");
typed_id!(AgentId, "agent");
typed_id!(SessionId, "session");
typed_id!(GenerationId, "gen");
typed_id!(ExperimentId, "exp");
typed_id!(ExecutionId, "exec");
typed_id!(ObservationId, "obs");
typed_id!(FindingId, "finding");
typed_id!(DecisionId, "decision");
typed_id!(ArtifactId, "artifact");
typed_id!(ProjectId, "project");

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    #[must_use]
    pub fn now() -> Self {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or_default();
        Self(millis)
    }

    #[must_use]
    pub fn from_millis(millis: i64) -> Self {
        Self(millis)
    }

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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObjectRef(String);

impl ObjectRef {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

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
