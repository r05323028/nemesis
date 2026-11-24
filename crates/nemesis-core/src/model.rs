use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub const MODEL_SCHEMA_VERSION: &str = "nemesis.v1";

fn default_schema_version() -> String {
    MODEL_SCHEMA_VERSION.to_owned()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceReference {
    pub path: String,
    pub locator: Option<String>,
    pub revision: Option<String>,
}

impl SourceReference {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            locator: None,
            revision: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Provenance {
    pub source: SourceReference,
    pub method: String,
    pub collected_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Requirement {
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
    pub id: String,
    pub source: SourceReference,
    pub description: String,
    pub provenance: Vec<Provenance>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntityReference {
    pub kind: EntityKind,
    pub id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Requirement,
    Specification,
    DesignDecision,
    Task,
    SourceCode,
    Test,
    Finding,
    ReviewRun,
    Evidence,
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TraceLink {
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
    pub source: EntityReference,
    pub target: EntityReference,
    pub relationship: TraceRelationship,
    pub evidence_refs: Vec<Uuid>,
    pub validation_state: ValidationState,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TraceRelationship {
    Satisfies,
    Specifies,
    Justifies,
    Implements,
    Tests,
    Verifies,
    Supports,
    Contradicts,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValidationState {
    Pending,
    Valid,
    Invalid,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Evidence {
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
    pub id: Uuid,
    pub evidence_type: EvidenceType,
    pub source: SourceReference,
    pub content: Option<String>,
    pub content_hash: Option<String>,
    pub git_revision: Option<String>,
    pub collection: CollectionMetadata,
    pub validation: EvidenceValidation,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceType {
    Artifact,
    Source,
    Test,
    CommandOutput,
    ReviewerClaim,
    ProcessEvent,
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CollectionMetadata {
    pub collector: String,
    pub collected_at: DateTime<Utc>,
    pub command: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceValidation {
    pub state: ValidationState,
    pub validated_at: Option<DateTime<Utc>>,
    pub validator: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Finding {
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
    pub id: String,
    pub claim: String,
    pub category: String,
    pub severity: Severity,
    pub source_refs: Vec<SourceReference>,
    pub evidence_refs: Vec<Uuid>,
    pub verification_state: VerificationState,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Informational,
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationState {
    Unverified,
    Confirmed,
    Rejected,
    Inconclusive,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewRun {
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
    pub run_id: Uuid,
    pub parent_run_id: Option<Uuid>,
    pub agent_runtime: String,
    pub reviewer_profile: String,
    pub lifecycle_status: LifecycleStatus,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub final_result_ref: Option<Uuid>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleStatus {
    Queued,
    Starting,
    Running,
    Verifying,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

impl LifecycleStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::TimedOut
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewerEvent {
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
    pub event_id: Uuid,
    pub run_id: Uuid,
    pub sequence: u64,
    pub timestamp: DateTime<Utc>,
    pub event_type: ReviewerEventType,
    pub phase: ExecutionPhase,
    pub payload: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewerEventType {
    RunQueued,
    RunStarted,
    AgentEvent,
    ProcessExited,
    RunCompleted,
    RunFailed,
    RunCancelled,
    RunTimedOut,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPhase {
    Queued,
    Starting,
    Reviewing,
    Verifying,
    Finalizing,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewResult {
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
    pub result_id: Uuid,
    pub run_id: Uuid,
    pub summary: Option<String>,
    pub findings: Vec<Finding>,
    pub evidence: Vec<Evidence>,
    pub raw_output: String,
    pub structured: bool,
    pub parse_error: Option<String>,
    pub completed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GateResult {
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
    pub policy_id: String,
    pub policy_version: String,
    pub input_evidence_refs: Vec<Uuid>,
    pub outcome: GateOutcome,
    pub reasons: Vec<String>,
    pub evaluated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GateOutcome {
    Pass,
    Fail,
    Inconclusive,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_models_round_trip_with_version() {
        let now = Utc::now();
        let requirement = Requirement {
            schema_version: MODEL_SCHEMA_VERSION.to_owned(),
            id: "REQ-001".to_owned(),
            source: SourceReference::new("openspec/changes/example/specs/auth/spec.md"),
            description: "Users must authenticate before viewing private data.".to_owned(),
            provenance: vec![Provenance {
                source: SourceReference::new("openspec/changes/example/proposal.md"),
                method: "openspec-artifact".to_owned(),
                collected_at: now,
            }],
        };
        let encoded = serde_json::to_string(&requirement).expect("serialize requirement");
        let decoded: Requirement = serde_json::from_str(&encoded).expect("deserialize requirement");
        assert_eq!(decoded, requirement);
        assert_eq!(decoded.schema_version, MODEL_SCHEMA_VERSION);
    }

    #[test]
    fn lifecycle_terminal_states_are_explicit() {
        assert!(!LifecycleStatus::Running.is_terminal());
        assert!(LifecycleStatus::Cancelled.is_terminal());
        assert!(LifecycleStatus::TimedOut.is_terminal());
    }

    #[test]
    fn enum_wire_names_are_stable() {
        let value = serde_json::to_value(GateOutcome::Inconclusive).expect("serialize outcome");
        assert_eq!(value, serde_json::json!("inconclusive"));
        let value = serde_json::to_value(ExecutionPhase::Reviewing).expect("serialize phase");
        assert_eq!(value, serde_json::json!("reviewing"));
    }
}
