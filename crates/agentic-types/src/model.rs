use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::{
    AgentId, ArtifactId, ExecutionId, ExperimentId, FlowId, GenerationId, GoalId, ObjectRef,
    SessionId, Timestamp,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FlowState {
    Foreground,

    Active,

    Quiescent,

    Hibernated,

    Archived,
}

impl FlowState {
    pub const ALL: [FlowState; 5] = [
        FlowState::Foreground,
        FlowState::Active,
        FlowState::Quiescent,
        FlowState::Hibernated,
        FlowState::Archived,
    ];

    #[must_use]
    pub fn initial() -> Self {
        FlowState::Active
    }
}

impl fmt::Display for FlowState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            FlowState::Foreground => "FOREGROUND",
            FlowState::Active => "ACTIVE",
            FlowState::Quiescent => "QUIESCENT",
            FlowState::Hibernated => "HIBERNATED",
            FlowState::Archived => "ARCHIVED",
        };
        f.write_str(value)
    }
}

impl FromStr for FlowState {
    type Err = StateParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_uppercase().as_str() {
            "FOREGROUND" => Ok(FlowState::Foreground),
            "ACTIVE" => Ok(FlowState::Active),
            "QUIESCENT" => Ok(FlowState::Quiescent),
            "HIBERNATED" => Ok(FlowState::Hibernated),
            "ARCHIVED" => Ok(FlowState::Archived),
            other => Err(StateParseError::Flow(other.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flow {
    pub id: FlowId,

    pub name: String,

    pub purpose: Option<String>,

    pub parent_flow: Option<FlowId>,

    pub state: FlowState,

    pub created_by: String,

    pub created_at: Timestamp,

    pub last_active: Option<Timestamp>,

    pub revision: u32,
}

impl Flow {
    #[must_use]
    pub fn new(name: impl Into<String>, created_by: impl Into<String>) -> Self {
        Self {
            id: FlowId::new(),
            name: name.into(),
            purpose: None,
            parent_flow: None,
            state: FlowState::initial(),
            created_by: created_by.into(),
            created_at: Timestamp::now(),
            last_active: None,
            revision: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GoalStatus {
    Proposed,

    Active,

    Suspended,

    Completed,

    Abandoned,

    Failed,
}

impl fmt::Display for GoalStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            GoalStatus::Proposed => "PROPOSED",
            GoalStatus::Active => "ACTIVE",
            GoalStatus::Suspended => "SUSPENDED",
            GoalStatus::Completed => "COMPLETED",
            GoalStatus::Abandoned => "ABANDONED",
            GoalStatus::Failed => "FAILED",
        };
        f.write_str(value)
    }
}

impl FromStr for GoalStatus {
    type Err = StateParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_uppercase().as_str() {
            "PROPOSED" => Ok(GoalStatus::Proposed),
            "ACTIVE" => Ok(GoalStatus::Active),
            "SUSPENDED" => Ok(GoalStatus::Suspended),
            "COMPLETED" => Ok(GoalStatus::Completed),
            "ABANDONED" => Ok(GoalStatus::Abandoned),
            "FAILED" => Ok(GoalStatus::Failed),
            other => Err(StateParseError::Goal(other.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    pub id: GoalId,

    pub parent_goal: Option<GoalId>,

    pub objective: String,

    pub rationale: Option<String>,

    pub constraints: Vec<String>,

    pub success_metrics: Vec<String>,

    pub priority: i32,

    pub status: GoalStatus,

    pub created_by: String,

    pub created_at: Timestamp,
}

impl Goal {
    #[must_use]
    pub fn new(objective: impl Into<String>, created_by: impl Into<String>) -> Self {
        Self {
            id: GoalId::new(),
            parent_goal: None,
            objective: objective.into(),
            rationale: None,
            constraints: Vec::new(),
            success_metrics: Vec::new(),
            priority: 0,
            status: GoalStatus::Proposed,
            created_by: created_by.into(),
            created_at: Timestamp::now(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SessionState {
    Running,

    Suspended,

    Exited,
}

impl fmt::Display for SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            SessionState::Running => "RUNNING",
            SessionState::Suspended => "SUSPENDED",
            SessionState::Exited => "EXITED",
        };
        f.write_str(value)
    }
}

impl FromStr for SessionState {
    type Err = StateParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_uppercase().as_str() {
            "RUNNING" => Ok(SessionState::Running),
            "SUSPENDED" => Ok(SessionState::Suspended),
            "EXITED" => Ok(SessionState::Exited),
            other => Err(StateParseError::Session(other.to_owned())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SessionPresentationState {
    Attached,

    Detached,
}

impl fmt::Display for SessionPresentationState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            SessionPresentationState::Attached => "ATTACHED",
            SessionPresentationState::Detached => "DETACHED",
        };
        f.write_str(value)
    }
}

impl FromStr for SessionPresentationState {
    type Err = StateParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_uppercase().as_str() {
            "ATTACHED" => Ok(SessionPresentationState::Attached),
            "DETACHED" => Ok(SessionPresentationState::Detached),
            other => Err(StateParseError::SessionPresentation(other.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,

    pub flow_id: Option<FlowId>,

    pub kind: String,

    pub pid: Option<i32>,

    pub cwd: Option<String>,

    /// Runtime lifecycle of the underlying process/session.
    pub state: SessionState,

    /// Whether a user-facing presentation is currently attached.
    pub presentation_state: SessionPresentationState,

    pub created_by: String,

    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Execution {
    pub id: ExecutionId,

    pub agent_id: Option<AgentId>,

    pub goal_id: Option<GoalId>,

    pub reason: Option<String>,

    pub command: Option<String>,

    pub exit_code: Option<i32>,

    pub duration_ms: Option<i64>,

    pub started_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GenerationStatus {
    Candidate,

    Tested,

    Stable,

    Rejected,

    Superseded,
}

impl fmt::Display for GenerationStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            GenerationStatus::Candidate => "CANDIDATE",
            GenerationStatus::Tested => "TESTED",
            GenerationStatus::Stable => "STABLE",
            GenerationStatus::Rejected => "REJECTED",
            GenerationStatus::Superseded => "SUPERSEDED",
        };
        f.write_str(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Generation {
    pub id: GenerationId,

    pub parent: Option<GenerationId>,

    pub source_commit: Option<String>,

    pub system_manifest: Option<String>,

    pub reason_goal: Option<GoalId>,

    pub experiment: Option<ExperimentId>,

    pub status: GenerationStatus,

    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceEdge {
    pub from: ObjectRef,

    pub relation: ProvenanceRelation,

    pub to: ObjectRef,

    pub created_at: Timestamp,

    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceRelation {
    CreatedBy,

    CausedBy,

    BelongsTo,

    Produced,

    TestedBy,

    PromotedTo,
}

impl ProvenanceRelation {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            ProvenanceRelation::CreatedBy => "created_by",
            ProvenanceRelation::CausedBy => "caused_by",
            ProvenanceRelation::BelongsTo => "belongs_to",
            ProvenanceRelation::Produced => "produced",
            ProvenanceRelation::TestedBy => "tested_by",
            ProvenanceRelation::PromotedTo => "promoted_to",
        }
    }
}

impl fmt::Display for ProvenanceRelation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub name: String,

    pub description: Option<String>,

    pub granted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: ArtifactId,

    pub kind: String,

    pub uri: String,

    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StateParseError {
    #[error("invalid flow state: `{0}`")]
    Flow(String),

    #[error("invalid goal status: `{0}`")]
    Goal(String),

    #[error("invalid session state: `{0}`")]
    Session(String),

    #[error("invalid session presentation state: `{0}`")]
    SessionPresentation(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flow_state_round_trips() {
        for state in FlowState::ALL {
            let parsed: FlowState = state.to_string().parse().expect("valid state");
            assert_eq!(state, parsed);
        }
    }

    #[test]
    fn a_new_flow_is_active() {
        let flow = Flow::new("Development", "user");
        assert_eq!(flow.state, FlowState::Active);
        assert_eq!(flow.revision, 1);
        assert!(flow.name.starts_with("Development"));
    }

    #[test]
    fn goal_serializes_round_trip() {
        let mut goal = Goal::new("Reduce build latency", "agent:planner");
        goal.status = GoalStatus::Active;
        goal.priority = 7;
        let json = serde_json::to_string(&goal).expect("serialize goal");
        let restored: Goal = serde_json::from_str(&json).expect("deserialize goal");
        assert_eq!(goal, restored);
    }

    #[test]
    fn session_runtime_and_presentation_states_are_independent() {
        assert_eq!(
            "RUNNING".parse::<SessionState>().expect("runtime state"),
            SessionState::Running
        );
        assert_eq!(
            "DETACHED"
                .parse::<SessionPresentationState>()
                .expect("presentation state"),
            SessionPresentationState::Detached
        );
    }

    #[test]
    fn provenance_relation_names_are_stable() {
        assert_eq!(ProvenanceRelation::CreatedBy.as_str(), "created_by");
        assert_eq!(ProvenanceRelation::PromotedTo.as_str(), "promoted_to");
    }
}
