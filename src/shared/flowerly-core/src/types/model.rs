use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use super::{
    AgentId, ArtifactId, ExecutionId, ExperimentId, FlowId, GenerationId, GoalId, ModelId,
    ObjectRef, ProfileId, ProviderId, SessionId, Timestamp, UsageId, WorkloadId,
};

/// Lifecycle of a Flow row. Which Flow is a caller's foreground is not a
/// lifecycle state: it is per owner (`flow_foreground`, #225).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FlowState {
    Active,

    Quiescent,

    Hibernated,

    Archived,
}

impl FlowState {
    pub const ALL: [FlowState; 4] = [
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

    /// Authoritative owner (#225); `created_by` is audit text. Never
    /// serialized, and deserializes as `Unresolved` (fail closed).
    #[serde(skip)]
    pub owner: super::Owner,

    pub last_active: Option<Timestamp>,

    pub revision: u32,
}

impl Flow {
    #[must_use]
    pub fn new(name: impl Into<String>, created_by: impl Into<String>) -> Self {
        Self {
            owner: super::Owner::Unresolved,
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

    /// Authoritative owner (#225); `created_by` is audit text. Never
    /// serialized, and deserializes as `Unresolved` (fail closed).
    #[serde(skip)]
    pub owner: super::Owner,
}

impl Goal {
    #[must_use]
    pub fn new(objective: impl Into<String>, created_by: impl Into<String>) -> Self {
        Self {
            owner: super::Owner::Unresolved,
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

    /// Authoritative owner (#225); `created_by` is audit text. Never
    /// serialized, and deserializes as `Unresolved` (fail closed).
    #[serde(skip)]
    pub owner: super::Owner,
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

    /// Authoritative owner (#225); `created_by` is audit text. Never
    /// serialized, and deserializes as `Unresolved` (fail closed).
    #[serde(skip)]
    pub owner: super::Owner,
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

impl FromStr for GenerationStatus {
    type Err = StateParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_uppercase().as_str() {
            "CANDIDATE" => Ok(GenerationStatus::Candidate),
            "TESTED" => Ok(GenerationStatus::Tested),
            "STABLE" => Ok(GenerationStatus::Stable),
            "REJECTED" => Ok(GenerationStatus::Rejected),
            "SUPERSEDED" => Ok(GenerationStatus::Superseded),
            other => Err(StateParseError::Generation(other.to_owned())),
        }
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

    /// Authoritative owner (#225); `created_by` is audit text. Never
    /// serialized, and deserializes as `Unresolved` (fail closed).
    #[serde(skip)]
    pub owner: super::Owner,
}

/// Execution substrate a model was actually measured on. Honest labeling:
/// `Unknown` must never be presented as a supported backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ModelBackend {
    Cpu,

    Cuda,

    Vulkan,

    Rocm,

    Npu,

    Unknown,
}

impl ModelBackend {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Cuda => "CUDA",
            Self::Vulkan => "VULKAN",
            Self::Rocm => "ROCM",
            Self::Npu => "NPU",
            Self::Unknown => "UNKNOWN",
        }
    }
}

impl fmt::Display for ModelBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ModelBackend {
    type Err = StateParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_uppercase().as_str() {
            "CPU" => Ok(Self::Cpu),
            "CUDA" => Ok(Self::Cuda),
            "VULKAN" => Ok(Self::Vulkan),
            "ROCM" => Ok(Self::Rocm),
            "NPU" => Ok(Self::Npu),
            "UNKNOWN" => Ok(Self::Unknown),
            other => Err(StateParseError::Backend(other.to_owned())),
        }
    }
}

/// A durable, de-duplicated model registry row. Every numeric claim must come
/// from a measured or explicitly recorded source; the catalog stores evidence
/// labels, not predictions. Retiring keeps provenance and history intact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRecord {
    pub id: ModelId,

    pub name: String,

    pub family: Option<String>,

    pub architecture: Option<String>,

    pub backend: ModelBackend,

    pub context_tokens: Option<u64>,

    pub measured_memory_bytes: Option<u64>,

    pub modalities: Vec<String>,

    pub license: Option<String>,

    pub evidence: Option<String>,

    pub created_by: String,

    pub created_at: Timestamp,

    pub retired_at: Option<Timestamp>,
}

impl ModelRecord {
    #[must_use]
    pub fn new(name: impl Into<String>, created_by: impl Into<String>) -> Self {
        Self {
            id: ModelId::new(),
            name: name.into(),
            family: None,
            architecture: None,
            backend: ModelBackend::Unknown,
            context_tokens: None,
            measured_memory_bytes: None,
            modalities: Vec::new(),
            license: None,
            evidence: None,
            created_by: created_by.into(),
            created_at: Timestamp::now(),
            retired_at: None,
        }
    }

    #[must_use]
    pub fn is_retired(&self) -> bool {
        self.retired_at.is_some()
    }
}

/// An admitted inference provider registration. Only capability labels live
/// here: no credentials, no grants and no remote endpoints until egress
/// policy authorization exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderRecord {
    pub id: ProviderId,

    pub name: String,

    pub kind: String,

    pub endpoint: Option<String>,

    pub capabilities: Vec<String>,

    pub created_by: String,

    pub created_at: Timestamp,

    pub retired_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WorkloadState {
    Queued,

    Admitted,

    Refused,

    Stopped,

    Completed,
}

impl WorkloadState {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "QUEUED",
            Self::Admitted => "ADMITTED",
            Self::Refused => "REFUSED",
            Self::Stopped => "STOPPED",
            Self::Completed => "COMPLETED",
        }
    }
}

impl FromStr for WorkloadState {
    type Err = StateParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_uppercase().as_str() {
            "QUEUED" => Ok(Self::Queued),
            "ADMITTED" => Ok(Self::Admitted),
            "REFUSED" => Ok(Self::Refused),
            "STOPPED" => Ok(Self::Stopped),
            "COMPLETED" => Ok(Self::Completed),
            other => Err(StateParseError::Workload(other.to_owned())),
        }
    }
}

impl fmt::Display for WorkloadState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One admission-decision ledger row, snapshotting the effective ceilings
/// applied at submit time. `reason` always explains the decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkloadRecord {
    pub id: WorkloadId,

    pub profile_id: ProfileId,

    pub profile_name: String,

    pub model_id: ModelId,

    pub background: crate::profile::BackgroundActivity,

    pub priority: i32,

    pub state: WorkloadState,

    pub reason: String,

    pub ceilings: serde_json::Value,

    pub created_by: String,

    pub created_at: Timestamp,

    /// Authoritative owner (#225); `created_by` is audit text. Never
    /// serialized, and deserializes as `Unresolved` (fail closed).
    #[serde(skip)]
    pub owner: super::Owner,

    pub admitted_at: Option<Timestamp>,

    pub stopped_at: Option<Timestamp>,

    pub completed_at: Option<Timestamp>,

    pub operation_id: Option<String>,
}

/// One usage-ledger entry (tokens/latency/cost). Credentials never appear;
/// the provider attribution is a label for the later provider boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRecord {
    pub id: UsageId,

    pub workload_id: WorkloadId,

    pub model_id: ModelId,

    pub tokens_in: u64,

    pub tokens_out: u64,

    pub latency_ms: u64,

    pub cost_micros: u64,

    pub created_by: String,

    pub created_at: Timestamp,

    /// Authoritative owner (#225); `created_by` is audit text. Never
    /// serialized, and deserializes as `Unresolved` (fail closed).
    #[serde(skip)]
    pub owner: super::Owner,
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

impl FromStr for ProvenanceRelation {
    type Err = StateParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "created_by" => Ok(ProvenanceRelation::CreatedBy),
            "caused_by" => Ok(ProvenanceRelation::CausedBy),
            "belongs_to" => Ok(ProvenanceRelation::BelongsTo),
            "produced" => Ok(ProvenanceRelation::Produced),
            "tested_by" => Ok(ProvenanceRelation::TestedBy),
            "promoted_to" => Ok(ProvenanceRelation::PromotedTo),
            other => Err(StateParseError::ProvenanceRelation(other.to_owned())),
        }
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

    #[error("invalid generation status: `{0}`")]
    Generation(String),

    #[error("invalid model backend: `{0}`")]
    Backend(String),

    #[error("invalid workload state: `{0}`")]
    Workload(String),

    #[error("invalid provenance relation: `{0}`")]
    ProvenanceRelation(String),
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
    fn generation_and_provenance_parsers_reject_unknown_values() {
        assert!("BROKEN".parse::<GenerationStatus>().is_err());
        assert!("made_up".parse::<ProvenanceRelation>().is_err());
    }

    #[test]
    fn provenance_relation_names_are_stable() {
        assert_eq!(ProvenanceRelation::CreatedBy.as_str(), "created_by");
        assert_eq!(ProvenanceRelation::PromotedTo.as_str(), "promoted_to");
    }
}
