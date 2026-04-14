use std::collections::BTreeMap;

use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

pub type Metadata = Value;

pub fn empty_object() -> Metadata {
    json!({})
}

pub fn now_utc() -> DateTime<Utc> {
    DateTime::<Utc>::from(SystemTime::now())
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    #[default]
    TextJournal,
    AudioDictation,
    ChatTurn,
    Import,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    #[default]
    Chunk,
    Transcript,
    Summary,
    Reflection,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    #[default]
    Semantic,
    Episodic,
    Procedural,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum MemorySubtype {
    #[default]
    Preference,
    Project,
    Habit,
    Person,
    Place,
    Goal,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum MemoryState {
    Candidate,
    #[default]
    Accepted,
    Superseded,
    Rejected,
    Deleted,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum ThreadKind {
    #[default]
    Project,
    LifeTheme,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum ThreadStatus {
    #[default]
    Active,
    Dormant,
    Closed,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum ProfileLabel {
    #[default]
    Identity,
    Preferences,
    ActiveThreads,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum GateDecision {
    #[default]
    NoInject,
    InjectCompact,
    DeferToTool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ConversationTurn {
    pub role: ConversationRole,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatThinkingMode {
    Adaptive,
    Enabled,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatThinkingEffort {
    Low,
    Medium,
    High,
    Max,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatModelOption {
    pub label: String,
    pub model_id: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub thinking_mode: Option<ChatThinkingMode>,
    #[serde(default)]
    pub thinking_effort: Option<ChatThinkingEffort>,
    #[serde(default)]
    pub thinking_budget_tokens: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatModelsResponse {
    pub backend: String,
    #[serde(default)]
    pub default_model_id: Option<String>,
    #[serde(default)]
    pub models: Vec<ChatModelOption>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub id: Uuid,
    pub kind: EntryKind,
    pub raw_text: Option<String>,
    pub asset_ref: Option<String>,
    pub captured_at: DateTime<Utc>,
    pub timezone: String,
    pub source_app: Option<String>,
    #[serde(default = "empty_object")]
    pub metadata: Metadata,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Artifact {
    pub id: Uuid,
    pub entry_id: Uuid,
    pub kind: ArtifactKind,
    pub ordinal: u32,
    pub display_text: String,
    pub retrieval_text: String,
    pub embedding: Option<EmbeddingVector>,
    #[serde(default = "empty_object")]
    pub metadata: Metadata,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Thread {
    pub id: Uuid,
    pub kind: ThreadKind,
    pub title: String,
    pub summary: String,
    pub status: ThreadStatus,
    #[serde(default = "empty_object")]
    pub metadata: Metadata,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MemoryRecord {
    pub id: Uuid,
    pub lineage_id: Uuid,
    pub kind: MemoryKind,
    pub subtype: MemorySubtype,
    pub display_text: String,
    pub retrieval_text: String,
    #[serde(default = "empty_object")]
    pub attrs: Metadata,
    pub observed_at: Option<DateTime<Utc>>,
    pub valid_from: DateTime<Utc>,
    pub valid_to: Option<DateTime<Utc>>,
    pub confidence: f32,
    pub salience: f32,
    pub state: MemoryState,
    pub embedding: Option<EmbeddingVector>,
    #[serde(default)]
    pub source_artifact_ids: Vec<Uuid>,
    pub thread_id: Option<Uuid>,
    pub parent_id: Option<Uuid>,
    pub path: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProfileBlock {
    pub label: ProfileLabel,
    pub text: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RetrievalTraceCandidate {
    pub memory_id: Uuid,
    pub lineage_id: Uuid,
    pub semantic_score: f32,
    pub lexical_score: f32,
    pub fusion_score: f32,
    pub temporal_bonus: f32,
    pub thread_bonus: f32,
    pub salience_bonus: f32,
    pub confidence_bonus: f32,
    pub reinjection_penalty: f32,
    pub stale_penalty: f32,
    pub final_score: f32,
    pub candidate_rank: usize,
    pub selected: bool,
    pub injected_rank: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RetrievalTrace {
    pub id: Uuid,
    pub query: String,
    pub recent_context: Option<String>,
    pub active_thread_id: Option<Uuid>,
    pub gate_decision: GateDecision,
    pub gate_confidence: f32,
    pub gate_reason: String,
    pub final_context: Option<String>,
    pub candidates: Vec<RetrievalTraceCandidate>,
    pub selected_memory_ids: Vec<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CaptureEntryResponse {
    pub entry: Entry,
    pub artifacts: Vec<Artifact>,
    pub memories: Vec<MemoryRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ScoredMemory {
    pub memory: MemoryRecord,
    pub semantic_score: f32,
    pub lexical_score: f32,
    pub fusion_score: f32,
    pub temporal_bonus: f32,
    pub thread_bonus: f32,
    pub salience_bonus: f32,
    pub confidence_bonus: f32,
    pub reinjection_penalty: f32,
    pub stale_penalty: f32,
    pub final_score: f32,
    pub prior_injected: bool,
    pub candidate_rank: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AssembleContextResponse {
    pub trace_id: Uuid,
    pub decision: GateDecision,
    pub gate_confidence: f32,
    pub gate_reason: String,
    pub context: Option<String>,
    pub selected_memories: Vec<MemoryRecord>,
    pub candidates: Vec<ScoredMemory>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatResponse {
    pub answer: String,
    pub trace_id: Uuid,
    pub injected_context: Option<String>,
    pub selected_memories: Vec<MemoryRecord>,
    #[serde(default)]
    pub model_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EmbeddingVector {
    pub values: Vec<f32>,
    pub model: Option<String>,
    pub device: Option<String>,
    pub source: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PreparedArtifactInput {
    pub kind: ArtifactKind,
    pub display_text: String,
    pub retrieval_text: String,
    pub embedding: Option<EmbeddingVector>,
    #[serde(default = "empty_object")]
    pub metadata: Metadata,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PreparedMemoryInput {
    pub kind: MemoryKind,
    pub subtype: MemorySubtype,
    pub display_text: String,
    pub retrieval_text: String,
    #[serde(default = "empty_object")]
    pub attrs: Metadata,
    pub observed_at: Option<DateTime<Utc>>,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    pub confidence: Option<f32>,
    pub salience: Option<f32>,
    pub state: Option<MemoryState>,
    pub embedding: Option<EmbeddingVector>,
    pub thread_title: Option<String>,
    #[serde(default)]
    pub source_artifact_ordinals: Vec<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CreateTextEntryRequest {
    pub raw_text: String,
    pub captured_at: Option<DateTime<Utc>>,
    pub timezone: Option<String>,
    pub source_app: Option<String>,
    #[serde(default)]
    pub prepared_artifacts: Vec<PreparedArtifactInput>,
    #[serde(default)]
    pub prepared_memories: Vec<PreparedMemoryInput>,
    #[serde(default = "empty_object")]
    pub metadata: Metadata,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CreateAudioEntryRequest {
    pub asset_ref: String,
    pub transcript_text: Option<String>,
    pub captured_at: Option<DateTime<Utc>>,
    pub timezone: Option<String>,
    pub source_app: Option<String>,
    #[serde(default)]
    pub prepared_artifacts: Vec<PreparedArtifactInput>,
    #[serde(default)]
    pub prepared_memories: Vec<PreparedMemoryInput>,
    #[serde(default = "empty_object")]
    pub metadata: Metadata,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SearchMemoriesRequest {
    pub query: String,
    pub recent_context: Option<String>,
    pub focus_from: Option<DateTime<Utc>>,
    pub focus_to: Option<DateTime<Utc>>,
    pub active_thread_id: Option<Uuid>,
    pub kind: Option<MemoryKind>,
    pub subtype: Option<MemorySubtype>,
    pub query_embedding: Option<EmbeddingVector>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AssembleContextRequest {
    pub query: String,
    #[serde(default)]
    pub recent_turns: Vec<ConversationTurn>,
    pub recent_context: Option<String>,
    pub active_thread_id: Option<Uuid>,
    pub focus_from: Option<DateTime<Utc>>,
    pub focus_to: Option<DateTime<Utc>>,
    pub query_embedding: Option<EmbeddingVector>,
    pub max_candidates: Option<usize>,
    pub max_injected: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PatchMemoryRequest {
    pub display_text: Option<String>,
    pub retrieval_text: Option<String>,
    pub attrs: Option<Metadata>,
    pub valid_to: Option<Option<DateTime<Utc>>>,
    pub confidence: Option<f32>,
    pub salience: Option<f32>,
    pub state: Option<MemoryState>,
    pub thread_id: Option<Option<Uuid>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatRespondRequest {
    pub message: String,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub recent_turns: Vec<ConversationTurn>,
    pub recent_context: Option<String>,
    pub active_thread_id: Option<Uuid>,
    pub focus_from: Option<DateTime<Utc>>,
    pub focus_to: Option<DateTime<Utc>>,
    pub query_embedding: Option<EmbeddingVector>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ApiErrorBody {
    pub error: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PersistedState {
    #[serde(default)]
    pub entries: BTreeMap<Uuid, Entry>,
    #[serde(default)]
    pub artifacts: BTreeMap<Uuid, Artifact>,
    #[serde(default)]
    pub memories: BTreeMap<Uuid, MemoryRecord>,
    #[serde(default)]
    pub threads: BTreeMap<Uuid, Thread>,
    #[serde(default)]
    pub profile_blocks: BTreeMap<ProfileLabel, ProfileBlock>,
    #[serde(default)]
    pub retrieval_traces: BTreeMap<Uuid, RetrievalTrace>,
}

impl Default for PersistedState {
    fn default() -> Self {
        let now = now_utc();
        let mut profile_blocks = BTreeMap::new();
        for (label, text) in [
            (ProfileLabel::Identity, "No stored identity context yet."),
            (
                ProfileLabel::Preferences,
                "No durable preference memories have been accepted yet.",
            ),
            (
                ProfileLabel::ActiveThreads,
                "No active project or life-theme threads are open yet.",
            ),
        ] {
            profile_blocks.insert(
                label,
                ProfileBlock {
                    label,
                    text: text.to_string(),
                    updated_at: now,
                },
            );
        }

        Self {
            entries: BTreeMap::new(),
            artifacts: BTreeMap::new(),
            memories: BTreeMap::new(),
            threads: BTreeMap::new(),
            profile_blocks,
            retrieval_traces: BTreeMap::new(),
        }
    }
}
