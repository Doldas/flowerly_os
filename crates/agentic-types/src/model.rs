//! Durable domain objects shared across Flowerly OS services and storage.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::{
    AgentId, ArtifactId, ExecutionId, ExperimentId, FlowId, GenerationId, GoalId, ObjectRef,
    SessionId, Timestamp,
};

/// Lifecycle state of a [`Flow`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FlowState {
    /// The user is currently interacting with the Flow.
    Foreground,
    /// Running normally in the background.
    Active,
    /// Interactive state preserved, expensive activity reduced.
    Quiescent,
    /// Most runtime state frozen or reconstructable.
    Hibernated,
    /// Retained for memory and provenance, not running.
    Archived,
}

impl FlowState {
    /// All states, in lifecycle order.
    pub const ALL: [FlowState; 5] = [
        FlowState::Foreground,
        FlowState::Active,
        FlowState::Quiescent,
        FlowState::Hibernated,
        FlowState::Archived,
    ];

    /// The initial state for a newly created Flow.
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

/// A persistent, evolvable operating environment representing one coherent
/// human activity context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flow {
    /// Stable identifier.
    pub id: FlowId,
    /// Human-facing name (e.g. `Development`).
    pub name: String,
    /// Free-form purpose statement.
    pub purpose: Option<String>,
    /// Parent Flow, if this is a nested context.
    pub parent_flow: Option<FlowId>,
    /// Lifecycle state.
    pub state: FlowState,
    /// Who or what created the Flow (`user`, `agent`, `system`).
    pub created_by: String,
    /// Creation time.
    pub created_at: Timestamp,
    /// Last time the Flow was foreground.
    pub last_active: Option<Timestamp>,
    /// Monotonic revision of the Flow's own configuration.
    pub revision: u32,
}

impl Flow {
    /// Create a new Flow in its initial state.
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

/// Lifecycle state of a [`Goal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GoalStatus {
    /// Created but not yet started.
    Proposed,
    /// Actively being pursued.
    Active,
    /// Temporarily paused.
    Suspended,
    /// Success criteria met.
    Completed,
    /// Deliberately abandoned.
    Abandoned,
    /// Failed and recorded as such.
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

/// A persistent goal — a first-class operating-system object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    /// Stable identifier.
    pub id: GoalId,
    /// Parent goal, used to build the goal graph.
    pub parent_goal: Option<GoalId>,
    /// What the goal is trying to achieve.
    pub objective: String,
    /// Why the goal exists.
    pub rationale: Option<String>,
    /// Hard constraints (serialized list).
    pub constraints: Vec<String>,
    /// Success criteria (serialized list).
    pub success_metrics: Vec<String>,
    /// Priority; higher means more important.
    pub priority: i32,
    /// Lifecycle status.
    pub status: GoalStatus,
    /// Who or what created the goal.
    pub created_by: String,
    /// Creation time.
    pub created_at: Timestamp,
}

impl Goal {
    /// Create a new goal owned by the given actor.
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

/// Lifecycle state of a [`Session`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SessionState {
    /// Process is running.
    Running,
    /// Process has exited.
    Exited,
    /// Session is detached from any presentation but alive.
    Detached,
}

impl fmt::Display for SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            SessionState::Running => "RUNNING",
            SessionState::Exited => "EXITED",
            SessionState::Detached => "DETACHED",
        };
        f.write_str(value)
    }
}

/// A persistent terminal/remote session. The window is only a presentation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// Stable identifier.
    pub id: SessionId,
    /// Flow this session belongs to.
    pub flow_id: Option<FlowId>,
    /// Kind of session (`local-pty`, `ssh`, `container`, `vm-console`).
    pub kind: String,
    /// Operating-system process id while running.
    pub pid: Option<i32>,
    /// Working directory the session was started in.
    pub cwd: Option<String>,
    /// Lifecycle state.
    pub state: SessionState,
    /// Who or what created the session.
    pub created_by: String,
    /// Creation time.
    pub created_at: Timestamp,
}

/// Result of an autonomous action, used for execution provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Execution {
    /// Stable identifier.
    pub id: ExecutionId,
    /// Agent that performed the action, if any.
    pub agent_id: Option<AgentId>,
    /// Goal this action served, if any.
    pub goal_id: Option<GoalId>,
    /// Human-readable reason.
    pub reason: Option<String>,
    /// The command or operation that was executed.
    pub command: Option<String>,
    /// Process exit code, when applicable.
    pub exit_code: Option<i32>,
    /// Duration in milliseconds.
    pub duration_ms: Option<i64>,
    /// Started at.
    pub started_at: Timestamp,
}

/// Status of a system [`Generation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GenerationStatus {
    /// Being built or tested.
    Candidate,
    /// Booted successfully in the Evolution World.
    Tested,
    /// Currently active on the machine.
    Stable,
    /// Rejected after evaluation.
    Rejected,
    /// Superseded by a newer stable generation.
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

/// A reproducible snapshot of system state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Generation {
    /// Stable identifier.
    pub id: GenerationId,
    /// Parent generation, if any.
    pub parent: Option<GenerationId>,
    /// Source commit this generation was built from.
    pub source_commit: Option<String>,
    /// Hash of the system manifest.
    pub system_manifest: Option<String>,
    /// Goal that motivated the generation.
    pub reason_goal: Option<GoalId>,
    /// Experiment that produced the generation.
    pub experiment: Option<ExperimentId>,
    /// Lifecycle status.
    pub status: GenerationStatus,
    /// Creation time.
    pub created_at: Timestamp,
}

/// A directed relationship between two durable objects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceEdge {
    /// The source endpoint.
    pub from: ObjectRef,
    /// The relationship.
    pub relation: ProvenanceRelation,
    /// The destination endpoint.
    pub to: ObjectRef,
    /// When the edge was recorded.
    pub created_at: Timestamp,
    /// Optional note explaining the relationship.
    pub note: Option<String>,
}

/// Relationship vocabulary for [`ProvenanceEdge`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceRelation {
    /// Actor created the object.
    CreatedBy,
    /// Object was caused by another.
    CausedBy,
    /// Object belongs to a parent context.
    BelongsTo,
    /// Actor produced an artifact or result.
    Produced,
    /// Object was tested by another.
    TestedBy,
    /// Candidate was promoted to a generation.
    PromotedTo,
}

impl ProvenanceRelation {
    /// The canonical stored value.
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

/// A named capability grant held by an agent or a Flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    /// Capability name (e.g. `system.service.restart`).
    pub name: String,
    /// Human-readable description.
    pub description: Option<String>,
    /// Whether the capability is currently granted.
    pub granted: bool,
}

/// Reference to a stored artifact (test report, benchmark, log excerpt).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    /// Stable identifier.
    pub id: ArtifactId,
    /// Kind (`test-report`, `benchmark`, `trace`, `stdout`).
    pub kind: String,
    /// URI or path where the artifact can be retrieved.
    pub uri: String,
    /// Creation time.
    pub created_at: Timestamp,
}

/// Error parsing a state value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StateParseError {
    /// Invalid flow state.
    #[error("invalid flow state: `{0}`")]
    Flow(String),
    /// Invalid goal status.
    #[error("invalid goal status: `{0}`")]
    Goal(String),
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
    fn provenance_relation_names_are_stable() {
        assert_eq!(ProvenanceRelation::CreatedBy.as_str(), "created_by");
        assert_eq!(ProvenanceRelation::PromotedTo.as_str(), "promoted_to");
    }
}
