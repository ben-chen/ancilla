use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, bail};
use chrono::{DateTime, Utc};
use serde_json::json;
use uuid::Uuid;

use crate::{
    bedrock::{
        ChatCompletionBackend, ChatCompletionRequest, ContextGateBackend, ContextGateRequest,
        ContextGateResult, SyntheticChatBackend,
    },
    embedder_client::Embedder,
    model::{
        Artifact, ArtifactKind, AssembleContextRequest, AssembleContextResponse,
        CaptureEntryResponse, ChatModelsResponse, ChatRespondRequest, ChatResponse,
        ConversationRole, CreateAudioEntryRequest, CreateMemoryRequest, CreateTextEntryRequest,
        EmbeddingVector, Entry, EntryKind, MemoryKind, MemoryRecord, MemoryState, MemorySubtype,
        PatchMemoryRequest, PersistedState, PreparedArtifactInput, PreparedMemoryInput,
        ProfileBlock, RetrievalTrace, ScoredMemory, SearchMemoriesRequest, Thread, ThreadKind,
        ThreadStatus, empty_object, now_utc,
    },
    retrieval::{
        SearchEnvironment, build_context_bundle, build_trace,
        gate_candidates as deterministic_gate_candidates, rank_memories, rebuild_profile_blocks,
    },
    store::SharedStore,
};

#[derive(Clone)]
pub struct AppService {
    store: SharedStore,
    chat_backend: Arc<dyn ChatCompletionBackend>,
    gate_backend: Option<Arc<dyn ContextGateBackend>>,
    embedder: Option<Arc<dyn Embedder>>,
    embedding_model: String,
}

#[derive(Clone, Debug)]
struct MemoryDraft {
    kind: MemoryKind,
    subtype: MemorySubtype,
    display_text: String,
    retrieval_text: String,
    attrs: serde_json::Value,
    observed_at: Option<DateTime<Utc>>,
    valid_from: Option<DateTime<Utc>>,
    valid_to: Option<DateTime<Utc>>,
    confidence: f32,
    salience: f32,
    state: MemoryState,
    embedding: Option<EmbeddingVector>,
    thread_title: Option<String>,
    source_artifact_ordinals: Vec<u32>,
}

impl AppService {
    pub async fn load(
        snapshot_path: Option<PathBuf>,
        database_url: Option<String>,
    ) -> anyhow::Result<Self> {
        Self::load_with_chat_backend_and_embedder(
            snapshot_path,
            database_url,
            Arc::new(SyntheticChatBackend),
            None,
            None,
            "perplexity-ai/pplx-embed-v1-0.6b".to_string(),
        )
        .await
    }

    pub async fn load_with_chat_backend(
        snapshot_path: Option<PathBuf>,
        database_url: Option<String>,
        chat_backend: Arc<dyn ChatCompletionBackend>,
    ) -> anyhow::Result<Self> {
        Self::load_with_chat_backend_and_embedder(
            snapshot_path,
            database_url,
            chat_backend,
            None,
            None,
            "perplexity-ai/pplx-embed-v1-0.6b".to_string(),
        )
        .await
    }

    pub async fn load_with_chat_backend_and_embedder(
        snapshot_path: Option<PathBuf>,
        database_url: Option<String>,
        chat_backend: Arc<dyn ChatCompletionBackend>,
        gate_backend: Option<Arc<dyn ContextGateBackend>>,
        embedder: Option<Arc<dyn Embedder>>,
        embedding_model: String,
    ) -> anyhow::Result<Self> {
        let store = SharedStore::load(snapshot_path, database_url).await?;
        Ok(Self {
            store,
            chat_backend,
            gate_backend,
            embedder,
            embedding_model,
        })
    }

    pub fn new_in_memory() -> Self {
        Self::new_in_memory_with_chat_backend(Arc::new(SyntheticChatBackend))
    }

    pub fn new_in_memory_with_chat_backend(chat_backend: Arc<dyn ChatCompletionBackend>) -> Self {
        Self {
            store: SharedStore::new_in_memory(),
            chat_backend,
            gate_backend: None,
            embedder: None,
            embedding_model: "perplexity-ai/pplx-embed-v1-0.6b".to_string(),
        }
    }

    pub async fn create_text_entry(
        &self,
        request: CreateTextEntryRequest,
    ) -> anyhow::Result<CaptureEntryResponse> {
        let prepared_artifacts = request.prepared_artifacts.clone();
        let prepared_memories = self
            .hydrate_prepared_memories(request.prepared_memories.clone())
            .await?;
        let captured_at = request.captured_at.unwrap_or_else(now_utc);
        let entry = Entry {
            id: Uuid::new_v4(),
            kind: EntryKind::Text,
            raw_text: Some(request.raw_text.clone()),
            asset_ref: None,
            captured_at,
            timezone: request.timezone.unwrap_or_else(|| "UTC".to_string()),
            source_app: request.source_app,
            metadata: merge_attrs(request.metadata, json!({ "source_modality": "text" })),
            created_at: now_utc(),
        };
        let response = self
            .capture_entry(entry, prepared_artifacts, prepared_memories)
            .await?;
        Ok(response)
    }

    pub async fn create_audio_entry(
        &self,
        request: CreateAudioEntryRequest,
    ) -> anyhow::Result<CaptureEntryResponse> {
        let prepared_artifacts = request.prepared_artifacts.clone();
        let prepared_memories = self
            .hydrate_prepared_memories(request.prepared_memories.clone())
            .await?;
        let captured_at = request.captured_at.unwrap_or_else(now_utc);
        let entry = Entry {
            id: Uuid::new_v4(),
            kind: EntryKind::Text,
            raw_text: request.transcript_text.clone(),
            asset_ref: Some(request.asset_ref),
            captured_at,
            timezone: request.timezone.unwrap_or_else(|| "UTC".to_string()),
            source_app: request.source_app,
            metadata: merge_attrs(request.metadata, json!({ "source_modality": "audio" })),
            created_at: now_utc(),
        };
        let response = self
            .capture_entry(entry, prepared_artifacts, prepared_memories)
            .await?;
        Ok(response)
    }

    pub async fn create_memory(
        &self,
        request: CreateMemoryRequest,
    ) -> anyhow::Result<CaptureEntryResponse> {
        let display_text = request.display_text.trim().to_string();
        if display_text.is_empty() {
            bail!("memory display_text cannot be empty");
        }

        let memory = PreparedMemoryInput {
            kind: request.kind,
            subtype: request.subtype,
            display_text: display_text.clone(),
            retrieval_text: request
                .retrieval_text
                .clone()
                .unwrap_or_else(|| display_text.clone()),
            attrs: request.attrs,
            observed_at: request.observed_at,
            valid_from: request.valid_from,
            valid_to: request.valid_to,
            confidence: request.confidence,
            salience: request.salience,
            state: Some(MemoryState::Accepted),
            embedding: None,
            thread_title: request.thread_title,
            source_artifact_ordinals: vec![0],
        };

        self.create_text_entry(CreateTextEntryRequest {
            raw_text: display_text,
            captured_at: request.captured_at,
            timezone: request.timezone,
            source_app: request.source_app,
            prepared_artifacts: Vec::new(),
            prepared_memories: vec![memory],
            metadata: merge_attrs(
                request.metadata,
                json!({ "capture_type": "explicit_memory" }),
            ),
        })
        .await
    }

    pub async fn list_timeline(&self) -> Vec<Entry> {
        let state = self.store.read_clone().await;
        let mut entries = state.entries.values().cloned().collect::<Vec<_>>();
        entries.sort_by(|left, right| right.captured_at.cmp(&left.captured_at));
        entries
    }

    pub async fn review_memories(&self) -> Vec<MemoryRecord> {
        let state = self.store.read_clone().await;
        let mut memories = state.memories.values().cloned().collect::<Vec<_>>();
        memories.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        memories
    }

    pub async fn search_memories(
        &self,
        request: SearchMemoriesRequest,
    ) -> anyhow::Result<Vec<ScoredMemory>> {
        let state = self.store.read_clone().await;
        let now = now_utc();
        let mut assemble_request = AssembleContextRequest {
            query: request.query,
            recent_turns: Vec::new(),
            recent_context: request.recent_context,
            gate_model_id: None,
            conversation_id: request.conversation_id,
            active_thread_id: request.active_thread_id,
            focus_from: request.focus_from,
            focus_to: request.focus_to,
            query_embedding: request.query_embedding,
            max_candidates: request.limit,
            max_injected: None,
        };
        assemble_request.query_embedding = self
            .hydrate_query_embedding(&assemble_request)
            .await?
            .or(assemble_request.query_embedding);

        let search_limit = request.limit.unwrap_or(20);
        let mut candidates = rank_memories(
            SearchEnvironment {
                memories: &state.memories,
                threads: &state.threads,
                retrieval_traces: &state.retrieval_traces,
            },
            &assemble_request,
            search_limit,
            now,
        );

        if let Some(kind) = request.kind {
            candidates.retain(|candidate| candidate.memory.kind == kind);
        }
        if let Some(subtype) = request.subtype {
            candidates.retain(|candidate| candidate.memory.subtype == subtype);
        }
        Ok(candidates)
    }

    pub async fn assemble_context(
        &self,
        request: AssembleContextRequest,
    ) -> anyhow::Result<AssembleContextResponse> {
        let state = self.store.read_clone().await;
        let now = now_utc();
        let max_candidates = request.max_candidates.unwrap_or(20);
        let max_injected = request.max_injected.unwrap_or(3);
        let mut request = request;
        request.query_embedding = self
            .hydrate_query_embedding(&request)
            .await?
            .or(request.query_embedding);
        let candidates = rank_memories(
            SearchEnvironment {
                memories: &state.memories,
                threads: &state.threads,
                retrieval_traces: &state.retrieval_traces,
            },
            &request,
            max_candidates,
            now,
        );
        let gate = self
            .gate_candidates(&request, &candidates, max_injected)
            .await?;
        let context = build_context_bundle(&gate.selected, &state.entries, &state.artifacts);
        let recent_context =
            collapse_recent_context(&request.recent_turns, request.recent_context.clone());
        let trace = build_trace(
            request.query.clone(),
            recent_context,
            request.conversation_id,
            request.active_thread_id,
            &gate,
            context.clone(),
            &candidates,
            now,
        );
        let trace_id = trace.id;
        self.store
            .write_with(|state| {
                state.retrieval_traces.insert(trace.id, trace);
            })
            .await?;

        Ok(AssembleContextResponse {
            trace_id,
            decision: gate.decision,
            gate_confidence: gate.confidence,
            gate_reason: gate.reason,
            context,
            selected_memories: gate
                .selected
                .into_iter()
                .map(|candidate| candidate.memory)
                .collect(),
            candidates,
        })
    }

    pub async fn patch_memory(
        &self,
        memory_id: Uuid,
        request: PatchMemoryRequest,
    ) -> anyhow::Result<MemoryRecord> {
        let updated = self
            .store
            .write_with(|state| -> anyhow::Result<MemoryRecord> {
                let memory = state
                    .memories
                    .get_mut(&memory_id)
                    .with_context(|| format!("memory {memory_id} not found"))?;
                if let Some(display_text) = request.display_text {
                    memory.display_text = display_text;
                }
                if let Some(retrieval_text) = request.retrieval_text {
                    memory.retrieval_text = retrieval_text;
                }
                if let Some(attrs) = request.attrs {
                    memory.attrs = attrs;
                }
                if let Some(valid_to) = request.valid_to {
                    memory.valid_to = valid_to;
                }
                if let Some(confidence) = request.confidence {
                    memory.confidence = confidence.clamp(0.0, 1.0);
                }
                if let Some(salience) = request.salience {
                    memory.salience = salience.clamp(0.0, 1.0);
                }
                if let Some(state_value) = request.state {
                    memory.state = state_value;
                }
                if let Some(thread_id) = request.thread_id {
                    memory.thread_id = thread_id;
                }
                memory.updated_at = now_utc();
                let updated = memory.clone();
                state.profile_blocks =
                    rebuild_profile_blocks(&state.memories, &state.threads, now_utc());
                Ok(updated)
            })
            .await??;
        Ok(updated)
    }

    pub async fn delete_memory(&self, memory_id: Uuid) -> anyhow::Result<MemoryRecord> {
        let deleted = self
            .store
            .write_with(|state| -> anyhow::Result<MemoryRecord> {
                let memory = state
                    .memories
                    .get_mut(&memory_id)
                    .with_context(|| format!("memory {memory_id} not found"))?;
                memory.state = MemoryState::Deleted;
                memory.valid_to = Some(now_utc());
                memory.updated_at = now_utc();
                let deleted = memory.clone();
                state.profile_blocks =
                    rebuild_profile_blocks(&state.memories, &state.threads, now_utc());
                Ok(deleted)
            })
            .await??;
        Ok(deleted)
    }

    pub async fn profile_blocks(&self) -> Vec<ProfileBlock> {
        let state = self.store.read_clone().await;
        state.profile_blocks.values().cloned().collect()
    }

    pub async fn retrieval_trace(&self, trace_id: Uuid) -> anyhow::Result<RetrievalTrace> {
        let state = self.store.read_clone().await;
        state
            .retrieval_traces
            .get(&trace_id)
            .cloned()
            .with_context(|| format!("trace {trace_id} not found"))
    }

    pub async fn chat_respond(&self, request: ChatRespondRequest) -> anyhow::Result<ChatResponse> {
        let recent_turns = request.recent_turns.clone();
        let recent_context = request.recent_context.clone();
        let context = self
            .assemble_context(AssembleContextRequest {
                query: request.message.clone(),
                recent_turns,
                recent_context: recent_context.clone(),
                gate_model_id: request.gate_model_id.clone(),
                conversation_id: request.conversation_id,
                active_thread_id: request.active_thread_id,
                focus_from: request.focus_from,
                focus_to: request.focus_to,
                query_embedding: request.query_embedding.clone(),
                max_candidates: Some(20),
                max_injected: Some(3),
            })
            .await?;

        let completion = self
            .chat_backend
            .complete(&ChatCompletionRequest {
                message: request.message.clone(),
                model_id: request.model_id.clone(),
                recent_turns: request.recent_turns.clone(),
                recent_context,
                injected_context: context.context.clone(),
                selected_memories: context.selected_memories.clone(),
                trace_id: context.trace_id,
            })
            .await?;

        self.store
            .write_with(|state| {
                let entry = Entry {
                    id: Uuid::new_v4(),
                    kind: EntryKind::ChatTurn,
                    raw_text: Some(request.message.clone()),
                    asset_ref: None,
                    captured_at: now_utc(),
                    timezone: "UTC".to_string(),
                    source_app: Some("chat".to_string()),
                    metadata: json!({
                        "role": ConversationRole::User,
                        "trace_id": context.trace_id,
                    }),
                    created_at: now_utc(),
                };
                state.entries.insert(entry.id, entry);
            })
            .await?;

        Ok(ChatResponse {
            answer: completion.answer,
            trace_id: context.trace_id,
            injected_context: context.context,
            selected_memories: context.selected_memories,
            model_id: completion.model_id,
        })
    }

    pub fn chat_models(&self) -> ChatModelsResponse {
        self.chat_backend.models()
    }

    pub async fn state(&self) -> PersistedState {
        self.store.read_clone().await
    }

    async fn hydrate_prepared_memories(
        &self,
        prepared_memories: Vec<PreparedMemoryInput>,
    ) -> anyhow::Result<Vec<PreparedMemoryInput>> {
        let Some(embedder) = &self.embedder else {
            return Ok(prepared_memories);
        };

        let mut missing_indexes = Vec::new();
        let mut texts = Vec::new();
        for (index, memory) in prepared_memories.iter().enumerate() {
            if memory.embedding.is_none() && !memory.retrieval_text.trim().is_empty() {
                missing_indexes.push(index);
                texts.push(memory.retrieval_text.clone());
            }
        }
        if missing_indexes.is_empty() {
            return Ok(prepared_memories);
        }

        let embeddings = embedder
            .embed_texts(&self.embedding_model, &texts, false)
            .await
            .with_context(|| "failed to embed prepared memories")?;

        let mut hydrated = prepared_memories;
        for (index, embedding) in missing_indexes.into_iter().zip(embeddings.into_iter()) {
            hydrated[index].embedding = Some(embedding);
        }
        Ok(hydrated)
    }

    async fn hydrate_query_embedding(
        &self,
        request: &AssembleContextRequest,
    ) -> anyhow::Result<Option<EmbeddingVector>> {
        if request.query_embedding.is_some() {
            return Ok(request.query_embedding.clone());
        }
        let Some(embedder) = &self.embedder else {
            return Ok(None);
        };

        let text = build_query_embedding_text(request);
        if text.trim().is_empty() {
            return Ok(None);
        }

        let mut embeddings = embedder
            .embed_texts(&self.embedding_model, &[text], false)
            .await
            .with_context(|| "failed to embed retrieval query")?;
        Ok(embeddings.pop())
    }

    async fn gate_candidates(
        &self,
        request: &AssembleContextRequest,
        candidates: &[ScoredMemory],
        max_injected: usize,
    ) -> anyhow::Result<crate::retrieval::GateResult> {
        let fallback = || deterministic_gate_candidates(&request.query, candidates, max_injected);

        let Some(gate_backend) = &self.gate_backend else {
            return Ok(fallback());
        };

        let gate_request = ContextGateRequest {
            query: request.query.clone(),
            recent_turns: request.recent_turns.clone(),
            recent_context: request.recent_context.clone(),
            candidates: candidates.to_vec(),
            max_injected,
            model_id: request.gate_model_id.clone(),
            trace_id: Uuid::new_v4(),
        };

        match gate_backend.gate(&gate_request).await {
            Ok(result) => match hydrate_gate_result(candidates, result) {
                Ok(gate) => Ok(gate),
                Err(error) => {
                    eprintln!("{error:#}");
                    if request.gate_model_id.is_some() {
                        Err(error.context("gate model returned an invalid decision"))
                    } else {
                        Ok(fallback())
                    }
                }
            },
            Err(error) => {
                eprintln!("{error:#}");
                if request.gate_model_id.is_some() {
                    Err(error.context("gate model request failed"))
                } else {
                    Ok(fallback())
                }
            }
        }
    }

    async fn capture_entry(
        &self,
        entry: Entry,
        prepared_artifacts: Vec<PreparedArtifactInput>,
        prepared_memories: Vec<PreparedMemoryInput>,
    ) -> anyhow::Result<CaptureEntryResponse> {
        let text = entry.raw_text.clone().unwrap_or_default();
        let artifacts = build_artifacts(&entry, &text, prepared_artifacts);
        let memory_drafts = prepared_memories
            .into_iter()
            .map(memory_draft_from_prepared)
            .collect::<Vec<_>>();

        let result = self
            .store
            .write_with(|state| -> anyhow::Result<CaptureEntryResponse> {
                state.entries.insert(entry.id, entry.clone());
                for artifact in &artifacts {
                    state.artifacts.insert(artifact.id, artifact.clone());
                }
                let memories = materialize_memories(state, &entry, &artifacts, memory_drafts)?;
                state.profile_blocks =
                    rebuild_profile_blocks(&state.memories, &state.threads, now_utc());
                Ok(CaptureEntryResponse {
                    entry: entry.clone(),
                    artifacts: artifacts.clone(),
                    memories,
                })
            })
            .await??;
        Ok(result)
    }
}

fn build_artifacts(
    entry: &Entry,
    text: &str,
    prepared_artifacts: Vec<PreparedArtifactInput>,
) -> Vec<Artifact> {
    let mut artifacts = if prepared_artifacts.is_empty() {
        chunk_text(text)
            .into_iter()
            .enumerate()
            .map(|(index, chunk)| Artifact {
                id: Uuid::new_v4(),
                entry_id: entry.id,
                kind: if entry_source_modality(entry) == Some("audio") && index == 0 {
                    ArtifactKind::Transcript
                } else {
                    ArtifactKind::Chunk
                },
                ordinal: index as u32,
                display_text: chunk.clone(),
                retrieval_text: chunk,
                embedding: None,
                metadata: empty_object(),
                created_at: now_utc(),
            })
            .collect::<Vec<_>>()
    } else {
        prepared_artifacts
            .into_iter()
            .enumerate()
            .map(|(index, artifact)| Artifact {
                id: Uuid::new_v4(),
                entry_id: entry.id,
                kind: artifact.kind,
                ordinal: index as u32,
                display_text: artifact.display_text,
                retrieval_text: artifact.retrieval_text,
                embedding: artifact.embedding,
                metadata: artifact.metadata,
                created_at: now_utc(),
            })
            .collect::<Vec<_>>()
    };

    let has_summary = artifacts
        .iter()
        .any(|artifact| artifact.kind == ArtifactKind::Summary);
    let has_reflection = artifacts
        .iter()
        .any(|artifact| artifact.kind == ArtifactKind::Reflection);
    if !text.trim().is_empty() && !has_summary {
        artifacts.push(Artifact {
            id: Uuid::new_v4(),
            entry_id: entry.id,
            kind: ArtifactKind::Summary,
            ordinal: artifacts.len() as u32,
            display_text: summarize_text(text),
            retrieval_text: text.to_string(),
            embedding: None,
            metadata: empty_object(),
            created_at: now_utc(),
        });
    }
    if !text.trim().is_empty() && !has_reflection {
        artifacts.push(Artifact {
            id: Uuid::new_v4(),
            entry_id: entry.id,
            kind: ArtifactKind::Reflection,
            ordinal: artifacts.len() as u32,
            display_text: reflect_text(text),
            retrieval_text: text.to_string(),
            embedding: None,
            metadata: empty_object(),
            created_at: now_utc(),
        });
    }

    artifacts
}

fn materialize_memories(
    state: &mut PersistedState,
    entry: &Entry,
    artifacts: &[Artifact],
    drafts: Vec<MemoryDraft>,
) -> anyhow::Result<Vec<MemoryRecord>> {
    let primary_artifact_ids = artifacts
        .iter()
        .filter(|artifact| {
            matches!(
                artifact.kind,
                ArtifactKind::Chunk | ArtifactKind::Transcript
            )
        })
        .map(|artifact| artifact.id)
        .collect::<Vec<_>>();
    let now = now_utc();
    let mut created = Vec::new();
    let artifact_id_by_ordinal = artifacts
        .iter()
        .map(|artifact| (artifact.ordinal, artifact.id))
        .collect::<std::collections::BTreeMap<_, _>>();

    for draft in drafts {
        let thread_id = if let Some(title) = &draft.thread_title {
            Some(resolve_thread(state, title, now))
        } else {
            None
        };
        let source_artifact_ids = if draft.source_artifact_ordinals.is_empty() {
            primary_artifact_ids.clone()
        } else {
            draft
                .source_artifact_ordinals
                .iter()
                .filter_map(|ordinal| artifact_id_by_ordinal.get(ordinal).copied())
                .collect::<Vec<_>>()
        };

        let memory = MemoryRecord {
            id: Uuid::new_v4(),
            lineage_id: Uuid::new_v4(),
            kind: draft.kind,
            subtype: draft.subtype,
            display_text: draft.display_text,
            retrieval_text: draft.retrieval_text,
            attrs: merge_attrs(
                draft.attrs,
                json!({
                    "source_entry_id": entry.id,
                }),
            ),
            observed_at: draft.observed_at.or(Some(entry.captured_at)),
            valid_from: draft.valid_from.unwrap_or(entry.captured_at),
            valid_to: draft.valid_to,
            confidence: draft.confidence,
            salience: draft.salience,
            state: draft.state,
            embedding: draft.embedding,
            source_artifact_ids,
            thread_id,
            parent_id: None,
            path: Some(format!("entries/{}/memories/{}", entry.id, created.len())),
            created_at: now,
            updated_at: now,
        };
        enforce_temporal_exclusivity(state, &memory)?;
        state.memories.insert(memory.id, memory.clone());
        created.push(memory);
    }

    Ok(created)
}

fn enforce_temporal_exclusivity(
    state: &mut PersistedState,
    memory: &MemoryRecord,
) -> anyhow::Result<()> {
    let overlaps = state.memories.values().any(|existing| {
        existing.lineage_id == memory.lineage_id
            && existing.state == MemoryState::Accepted
            && intervals_overlap(
                existing.valid_from,
                existing.valid_to,
                memory.valid_from,
                memory.valid_to,
            )
    });
    if overlaps {
        bail!(
            "memory lineage {} has overlapping accepted validity windows",
            memory.lineage_id
        );
    }
    Ok(())
}

fn resolve_thread(state: &mut PersistedState, title: &str, now: DateTime<Utc>) -> Uuid {
    if let Some(existing) = state
        .threads
        .values_mut()
        .find(|thread| thread.title.eq_ignore_ascii_case(title))
    {
        existing.status = ThreadStatus::Active;
        existing.updated_at = now;
        return existing.id;
    }

    let thread = Thread {
        id: Uuid::new_v4(),
        kind: ThreadKind::Project,
        title: title.to_string(),
        summary: format!("Thread derived from explicit memory: {title}"),
        status: ThreadStatus::Active,
        metadata: empty_object(),
        created_at: now,
        updated_at: now,
    };
    let id = thread.id;
    state.threads.insert(id, thread);
    id
}

fn chunk_text(text: &str) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    for sentence in text.split_terminator(['.', '\n']) {
        let sentence = sentence.trim();
        if sentence.is_empty() {
            continue;
        }
        if sentence.len() <= 280 {
            chunks.push(sentence.to_string());
        } else {
            for chunk in sentence.as_bytes().chunks(280) {
                chunks.push(String::from_utf8_lossy(chunk).trim().to_string());
            }
        }
    }
    chunks
}

fn summarize_text(text: &str) -> String {
    let first = text
        .split_terminator('.')
        .next()
        .unwrap_or(text)
        .trim()
        .chars()
        .take(160)
        .collect::<String>();
    format!("Summary: {first}")
}

fn reflect_text(text: &str) -> String {
    let summary = summarize_text(text);
    format!(
        "Reflection: the entry centers on {}",
        summary.trim_start_matches("Summary: ")
    )
}

fn memory_draft_from_prepared(prepared: PreparedMemoryInput) -> MemoryDraft {
    MemoryDraft {
        kind: prepared.kind,
        subtype: prepared.subtype,
        display_text: prepared.display_text,
        retrieval_text: prepared.retrieval_text,
        attrs: prepared.attrs,
        observed_at: prepared.observed_at,
        valid_from: prepared.valid_from,
        valid_to: prepared.valid_to,
        confidence: prepared.confidence.unwrap_or(0.8).clamp(0.0, 1.0),
        salience: prepared.salience.unwrap_or(0.8).clamp(0.0, 1.0),
        state: prepared.state.unwrap_or(MemoryState::Accepted),
        embedding: prepared.embedding,
        thread_title: prepared.thread_title,
        source_artifact_ordinals: prepared.source_artifact_ordinals,
    }
}

fn build_query_embedding_text(request: &AssembleContextRequest) -> String {
    let mut parts = Vec::new();
    if let Some(recent_context) = request.recent_context.as_deref()
        && !recent_context.trim().is_empty()
    {
        parts.push(recent_context.trim().to_string());
    }
    for turn in &request.recent_turns {
        if !turn.text.trim().is_empty() {
            parts.push(format!("{:?}: {}", turn.role, turn.text.trim()));
        }
    }
    if !request.query.trim().is_empty() {
        parts.push(format!("User: {}", request.query.trim()));
    }

    let tokens = parts
        .join("\n")
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let start = tokens.len().saturating_sub(500);
    tokens[start..].join(" ")
}

fn hydrate_gate_result(
    candidates: &[ScoredMemory],
    result: ContextGateResult,
) -> anyhow::Result<crate::retrieval::GateResult> {
    let candidate_map = candidates
        .iter()
        .map(|candidate| (candidate.memory.id, candidate.clone()))
        .collect::<std::collections::HashMap<_, _>>();
    let selected = result
        .selected_memory_ids
        .iter()
        .map(|memory_id| {
            candidate_map
                .get(memory_id)
                .cloned()
                .with_context(|| format!("gate selected unknown memory {memory_id}"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    Ok(crate::retrieval::GateResult {
        decision: result.decision,
        confidence: result.confidence,
        reason: result.reason,
        selected,
    })
}

fn merge_attrs(base: serde_json::Value, extra: serde_json::Value) -> serde_json::Value {
    match (base, extra) {
        (serde_json::Value::Object(mut base_map), serde_json::Value::Object(extra_map)) => {
            for (key, value) in extra_map {
                base_map.insert(key, value);
            }
            serde_json::Value::Object(base_map)
        }
        (base, _) => base,
    }
}

fn entry_source_modality(entry: &Entry) -> Option<&str> {
    entry.metadata.get("source_modality")?.as_str()
}

fn intervals_overlap(
    left_from: DateTime<Utc>,
    left_to: Option<DateTime<Utc>>,
    right_from: DateTime<Utc>,
    right_to: Option<DateTime<Utc>>,
) -> bool {
    let left_to = left_to.unwrap_or(DateTime::<Utc>::MAX_UTC);
    let right_to = right_to.unwrap_or(DateTime::<Utc>::MAX_UTC);
    left_from <= right_to && right_from <= left_to
}

fn collapse_recent_context(
    recent_turns: &[crate::model::ConversationTurn],
    explicit_context: Option<String>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(explicit_context) = explicit_context {
        parts.push(explicit_context);
    }
    if !recent_turns.is_empty() {
        parts.push(
            recent_turns
                .iter()
                .map(|turn| turn.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use chrono::{Duration, TimeZone};
    use tempfile::tempdir;

    use crate::model::{GateDecision, ProfileLabel, SearchMemoriesRequest};

    use super::*;

    #[derive(Clone, Default)]
    struct RecordingBackend {
        seen: Arc<Mutex<Vec<ChatCompletionRequest>>>,
    }

    #[async_trait]
    impl ChatCompletionBackend for RecordingBackend {
        async fn complete(
            &self,
            request: &ChatCompletionRequest,
        ) -> anyhow::Result<crate::bedrock::ChatCompletionResult> {
            self.seen.lock().unwrap().push(request.clone());
            Ok(crate::bedrock::ChatCompletionResult {
                answer: format!("bedrock-mock: {}", request.message),
                model_id: request.model_id.clone(),
            })
        }

        fn models(&self) -> ChatModelsResponse {
            ChatModelsResponse {
                backend: "bedrock".to_string(),
                default_model_id: Some("anthropic.claude-opus-4-6-v1".to_string()),
                models: Vec::new(),
            }
        }
    }

    #[tokio::test]
    async fn explicit_memory_capture_creates_artifacts_memories_and_profile_blocks() {
        let service = AppService::new_in_memory();
        let response = service
            .create_memory(CreateMemoryRequest {
                display_text: "You prefer Rust for backend services.".to_string(),
                retrieval_text: None,
                kind: MemoryKind::Semantic,
                subtype: MemorySubtype::Preference,
                captured_at: Some(Utc.with_ymd_and_hms(2026, 4, 13, 18, 0, 0).unwrap()),
                timezone: Some("America/Los_Angeles".to_string()),
                source_app: Some("test".to_string()),
                attrs: json!({}),
                observed_at: None,
                valid_from: None,
                valid_to: None,
                confidence: None,
                salience: None,
                thread_title: None,
                metadata: json!({}),
            })
            .await
            .unwrap();

        assert_eq!(response.artifacts.len(), 3);
        assert_eq!(response.memories.len(), 1);

        let blocks = service.profile_blocks().await;
        let preferences = blocks
            .iter()
            .find(|block| block.label == ProfileLabel::Preferences)
            .unwrap();
        assert!(
            preferences
                .text
                .contains("You prefer Rust for backend services.")
        );
    }

    #[tokio::test]
    async fn audio_capture_is_normalized_to_text_with_audio_metadata() {
        let service = AppService::new_in_memory();
        let response = service
            .create_audio_entry(CreateAudioEntryRequest {
                asset_ref: "s3://bucket/clip.m4a".to_string(),
                transcript_text: Some("I prefer voice notes for capture.".to_string()),
                captured_at: Some(Utc.with_ymd_and_hms(2026, 4, 13, 19, 0, 0).unwrap()),
                timezone: Some("UTC".to_string()),
                source_app: Some("test".to_string()),
                prepared_artifacts: Vec::new(),
                prepared_memories: Vec::new(),
                metadata: json!({}),
            })
            .await
            .unwrap();

        assert_eq!(response.entry.kind, EntryKind::Text);
        assert_eq!(
            response.entry.metadata["source_modality"].as_str(),
            Some("audio")
        );
        assert_eq!(response.artifacts[0].kind, ArtifactKind::Transcript);
    }

    #[tokio::test]
    async fn persistence_round_trip_preserves_state() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let service = AppService::load(Some(path.clone()), None).await.unwrap();
        service
            .create_text_entry(CreateTextEntryRequest {
                raw_text: "I prefer Rust.".to_string(),
                captured_at: None,
                timezone: None,
                source_app: None,
                prepared_artifacts: Vec::new(),
                prepared_memories: Vec::new(),
                metadata: json!({}),
            })
            .await
            .unwrap();

        let reloaded = AppService::load(Some(path), None).await.unwrap();
        let timeline = reloaded.list_timeline().await;
        assert_eq!(timeline.len(), 1);
    }

    #[tokio::test]
    async fn search_and_context_assembly_return_traceable_memory() {
        let service = AppService::new_in_memory();
        service
            .create_memory(CreateMemoryRequest {
                display_text: "You prefer Rust for backend services.".to_string(),
                retrieval_text: Some("backend language rust".to_string()),
                kind: MemoryKind::Semantic,
                subtype: MemorySubtype::Preference,
                captured_at: Some(now_utc() - Duration::days(1)),
                timezone: None,
                source_app: None,
                attrs: json!({}),
                observed_at: None,
                valid_from: None,
                valid_to: None,
                confidence: None,
                salience: None,
                thread_title: None,
                metadata: json!({}),
            })
            .await
            .unwrap();
        service
            .create_memory(CreateMemoryRequest {
                display_text: "You are building a personal memory system on AWS.".to_string(),
                retrieval_text: Some("building personal memory system aws".to_string()),
                kind: MemoryKind::Semantic,
                subtype: MemorySubtype::Project,
                captured_at: Some(now_utc() - Duration::days(1)),
                timezone: None,
                source_app: None,
                attrs: json!({}),
                observed_at: None,
                valid_from: None,
                valid_to: None,
                confidence: None,
                salience: None,
                thread_title: Some("Ancilla".to_string()),
                metadata: json!({}),
            })
            .await
            .unwrap();

        let results = service
            .search_memories(SearchMemoriesRequest {
                query: "What backend language should I use?".to_string(),
                recent_context: None,
                conversation_id: None,
                focus_from: None,
                focus_to: None,
                active_thread_id: None,
                kind: None,
                subtype: Some(MemorySubtype::Preference),
                query_embedding: None,
                limit: Some(5),
            })
            .await
            .unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].memory.subtype, MemorySubtype::Preference);

        let assembled = service
            .assemble_context(AssembleContextRequest {
                query: "What am I building?".to_string(),
                recent_turns: Vec::new(),
                recent_context: None,
                gate_model_id: None,
                conversation_id: None,
                active_thread_id: None,
                focus_from: None,
                focus_to: None,
                query_embedding: None,
                max_candidates: Some(10),
                max_injected: Some(2),
            })
            .await
            .unwrap();
        assert_eq!(assembled.decision, GateDecision::InjectCompact);
        assert!(
            assembled
                .context
                .unwrap()
                .contains("Relevant personal context")
        );

        let trace = service.retrieval_trace(assembled.trace_id).await.unwrap();
        assert!(!trace.candidates.is_empty());
    }

    #[tokio::test]
    async fn patch_and_delete_memory_update_visibility() {
        let service = AppService::new_in_memory();
        let created = service
            .create_memory(CreateMemoryRequest {
                display_text: "You prefer Rust.".to_string(),
                retrieval_text: None,
                kind: MemoryKind::Semantic,
                subtype: MemorySubtype::Preference,
                captured_at: None,
                timezone: None,
                source_app: None,
                attrs: json!({}),
                observed_at: None,
                valid_from: None,
                valid_to: None,
                confidence: None,
                salience: None,
                thread_title: None,
                metadata: json!({}),
            })
            .await
            .unwrap();
        let memory = created.memories.first().unwrap().clone();

        let patched = service
            .patch_memory(
                memory.id,
                PatchMemoryRequest {
                    display_text: Some("You prefer Rust and Axum.".to_string()),
                    retrieval_text: None,
                    attrs: None,
                    valid_to: None,
                    confidence: Some(0.99),
                    salience: None,
                    state: None,
                    thread_id: None,
                },
            )
            .await
            .unwrap();
        assert!(patched.display_text.contains("Axum"));

        let deleted = service.delete_memory(memory.id).await.unwrap();
        assert_eq!(deleted.state, MemoryState::Deleted);
    }

    #[tokio::test]
    async fn chat_response_includes_context_when_available() {
        let backend = RecordingBackend::default();
        let service = AppService::new_in_memory_with_chat_backend(Arc::new(backend.clone()));
        service
            .create_memory(CreateMemoryRequest {
                display_text: "You are building a personal memory system.".to_string(),
                retrieval_text: Some("building personal memory system".to_string()),
                kind: MemoryKind::Semantic,
                subtype: MemorySubtype::Project,
                captured_at: None,
                timezone: None,
                source_app: None,
                attrs: json!({}),
                observed_at: None,
                valid_from: None,
                valid_to: None,
                confidence: None,
                salience: None,
                thread_title: Some("Ancilla".to_string()),
                metadata: json!({}),
            })
            .await
            .unwrap();

        let response = service
            .chat_respond(ChatRespondRequest {
                message: "What am I building?".to_string(),
                model_id: Some("anthropic.claude-opus-4-6-v1".to_string()),
                gate_model_id: None,
                recent_turns: Vec::new(),
                recent_context: None,
                conversation_id: None,
                active_thread_id: None,
                focus_from: None,
                focus_to: None,
                query_embedding: None,
            })
            .await
            .unwrap();
        assert_eq!(response.answer, "bedrock-mock: What am I building?");
        assert_eq!(
            response.model_id.as_deref(),
            Some("anthropic.claude-opus-4-6-v1")
        );
        assert!(!response.selected_memories.is_empty());
        let seen = backend.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].model_id.as_deref(),
            Some("anthropic.claude-opus-4-6-v1")
        );
        assert!(
            seen[0]
                .injected_context
                .as_deref()
                .unwrap()
                .contains("personal memory system")
        );
    }

    #[tokio::test]
    async fn client_supplied_embeddings_drive_semantic_retrieval() {
        let service = AppService::new_in_memory();
        let query_embedding = EmbeddingVector {
            values: vec![1.0, 0.0, 0.0],
            model: Some("test-model".to_string()),
            device: Some("cpu".to_string()),
            source: Some("client".to_string()),
        };
        let matching_embedding = EmbeddingVector {
            values: vec![1.0, 0.0, 0.0],
            model: Some("test-model".to_string()),
            device: Some("cpu".to_string()),
            source: Some("client".to_string()),
        };
        let other_embedding = EmbeddingVector {
            values: vec![0.0, 1.0, 0.0],
            model: Some("test-model".to_string()),
            device: Some("cpu".to_string()),
            source: Some("client".to_string()),
        };

        service
            .create_text_entry(CreateTextEntryRequest {
                raw_text: "Client-prepared embeddings".to_string(),
                captured_at: None,
                timezone: None,
                source_app: Some("test".to_string()),
                prepared_artifacts: vec![PreparedArtifactInput {
                    kind: ArtifactKind::Chunk,
                    display_text: "I prefer Rust.".to_string(),
                    retrieval_text: "I prefer Rust.".to_string(),
                    embedding: Some(matching_embedding.clone()),
                    metadata: json!({ "source": "audio_transcript" }),
                }],
                prepared_memories: vec![
                    PreparedMemoryInput {
                        kind: MemoryKind::Semantic,
                        subtype: MemorySubtype::Preference,
                        display_text: "You prefer Rust.".to_string(),
                        retrieval_text: "preference rust".to_string(),
                        attrs: json!({}),
                        observed_at: None,
                        valid_from: None,
                        valid_to: None,
                        confidence: Some(0.95),
                        salience: Some(0.9),
                        state: Some(MemoryState::Accepted),
                        embedding: Some(matching_embedding),
                        thread_title: None,
                        source_artifact_ordinals: vec![0],
                    },
                    PreparedMemoryInput {
                        kind: MemoryKind::Semantic,
                        subtype: MemorySubtype::Preference,
                        display_text: "You prefer Go.".to_string(),
                        retrieval_text: "preference go".to_string(),
                        attrs: json!({}),
                        observed_at: None,
                        valid_from: None,
                        valid_to: None,
                        confidence: Some(0.95),
                        salience: Some(0.9),
                        state: Some(MemoryState::Accepted),
                        embedding: Some(other_embedding),
                        thread_title: None,
                        source_artifact_ordinals: vec![0],
                    },
                ],
                metadata: json!({}),
            })
            .await
            .unwrap();

        let results = service
            .search_memories(SearchMemoriesRequest {
                query: "Which backend language?".to_string(),
                recent_context: None,
                conversation_id: None,
                focus_from: None,
                focus_to: None,
                active_thread_id: None,
                kind: None,
                subtype: Some(MemorySubtype::Preference),
                query_embedding: Some(query_embedding),
                limit: Some(5),
            })
            .await
            .unwrap();

        assert_eq!(results[0].memory.display_text, "You prefer Rust.");
        assert!(results[0].semantic_score > results[1].semantic_score);
    }

    #[tokio::test]
    async fn postgres_hybrid_query_returns_ranked_candidates_when_test_database_url_is_present() {
        let Some(database_url) = env::var("TEST_DATABASE_URL").ok() else {
            return;
        };

        let service = match AppService::load(None, Some(database_url)).await {
            Ok(service) => service,
            Err(error) if error.to_string().contains("vector") => return,
            Err(error) => panic!("{error}"),
        };

        let unique_tag = format!("sql-hybrid-{}", Uuid::new_v4());
        let query_embedding = EmbeddingVector {
            values: vec![1.0, 0.0, 0.0],
            model: Some("test-model".to_string()),
            device: Some("cpu".to_string()),
            source: Some("client".to_string()),
        };

        service
            .create_text_entry(CreateTextEntryRequest {
                raw_text: format!("Prepared memory seed {unique_tag}"),
                captured_at: None,
                timezone: None,
                source_app: Some("test".to_string()),
                prepared_artifacts: vec![PreparedArtifactInput {
                    kind: ArtifactKind::Chunk,
                    display_text: format!("artifact {unique_tag}"),
                    retrieval_text: format!("artifact {unique_tag}"),
                    embedding: Some(query_embedding.clone()),
                    metadata: json!({ "source": "client_test" }),
                }],
                prepared_memories: vec![
                    PreparedMemoryInput {
                        kind: MemoryKind::Semantic,
                        subtype: MemorySubtype::Preference,
                        display_text: format!("You prefer Rust for {unique_tag}."),
                        retrieval_text: format!("preference {unique_tag}"),
                        attrs: json!({}),
                        observed_at: None,
                        valid_from: None,
                        valid_to: None,
                        confidence: Some(0.95),
                        salience: Some(0.9),
                        state: Some(MemoryState::Accepted),
                        embedding: Some(query_embedding.clone()),
                        thread_title: None,
                        source_artifact_ordinals: vec![0],
                    },
                    PreparedMemoryInput {
                        kind: MemoryKind::Semantic,
                        subtype: MemorySubtype::Preference,
                        display_text: format!("You prefer Go for {unique_tag}."),
                        retrieval_text: format!("preference {unique_tag}"),
                        attrs: json!({}),
                        observed_at: None,
                        valid_from: None,
                        valid_to: None,
                        confidence: Some(0.95),
                        salience: Some(0.9),
                        state: Some(MemoryState::Accepted),
                        embedding: Some(EmbeddingVector {
                            values: vec![0.0, 1.0, 0.0],
                            model: Some("test-model".to_string()),
                            device: Some("cpu".to_string()),
                            source: Some("client".to_string()),
                        }),
                        thread_title: None,
                        source_artifact_ordinals: vec![0],
                    },
                ],
                metadata: json!({}),
            })
            .await
            .unwrap();

        let results = service
            .search_memories(SearchMemoriesRequest {
                query: format!("Which preference matches {unique_tag}?"),
                recent_context: None,
                conversation_id: None,
                focus_from: None,
                focus_to: None,
                active_thread_id: None,
                kind: None,
                subtype: Some(MemorySubtype::Preference),
                query_embedding: Some(query_embedding),
                limit: Some(5),
            })
            .await
            .unwrap();

        assert_eq!(
            results[0].memory.display_text,
            format!("You prefer Rust for {unique_tag}.")
        );
        assert!(results[0].semantic_score >= results[1].semantic_score);
    }
}
