use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, bail};
use chrono::{DateTime, Utc};
use pgvector::Vector;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;
use sqlx::{
    FromRow, PgPool, Postgres, Transaction,
    postgres::{PgPoolOptions, types::PgInterval},
    types::Json,
};
use tokio::{fs, sync::RwLock};
use uuid::Uuid;

use crate::model::{
    Artifact, AssembleContextRequest, EmbeddingVector, Entry, GateDecision, MemoryRecord,
    PersistedState, ProfileBlock, RetrievalTrace, RetrievalTraceCandidate, ScoredMemory, Thread,
};
use crate::retrieval::build_query_material;

const HYBRID_MEMORY_CANDIDATES_SQL: &str = include_str!("../sql/hybrid_memory_candidates.sql");

#[derive(Clone, Debug)]
pub struct SharedStore {
    inner: Arc<RwLock<PersistedState>>,
    backend: PersistBackend,
}

#[derive(Clone, Debug)]
enum PersistBackend {
    Memory,
    JsonFile(PathBuf),
    Postgres(PostgresStore),
}

#[derive(Clone, Debug)]
struct PostgresStore {
    pool: PgPool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendKind {
    Memory,
    JsonFile,
    Postgres,
}

impl SharedStore {
    pub async fn load(
        snapshot_path: Option<PathBuf>,
        database_url: Option<String>,
    ) -> anyhow::Result<Self> {
        match database_url {
            Some(database_url) => {
                let backend = PostgresStore::connect(&database_url).await?;
                let state = backend.load_state().await?;
                Ok(Self {
                    inner: Arc::new(RwLock::new(state)),
                    backend: PersistBackend::Postgres(backend),
                })
            }
            None => {
                let backend = if let Some(path) = snapshot_path {
                    PersistBackend::JsonFile(path)
                } else {
                    PersistBackend::Memory
                };
                let state = load_state_from_backend(&backend).await?;
                Ok(Self {
                    inner: Arc::new(RwLock::new(state)),
                    backend,
                })
            }
        }
    }

    pub fn new_in_memory() -> Self {
        Self {
            inner: Arc::new(RwLock::new(PersistedState::default())),
            backend: PersistBackend::Memory,
        }
    }

    pub fn backend_kind(&self) -> BackendKind {
        match self.backend {
            PersistBackend::Memory => BackendKind::Memory,
            PersistBackend::JsonFile(_) => BackendKind::JsonFile,
            PersistBackend::Postgres(_) => BackendKind::Postgres,
        }
    }

    pub async fn read_clone(&self) -> PersistedState {
        self.inner.read().await.clone()
    }

    pub async fn search_candidates(
        &self,
        request: &AssembleContextRequest,
        limit: usize,
        now: DateTime<Utc>,
        state: &PersistedState,
    ) -> anyhow::Result<Option<Vec<ScoredMemory>>> {
        match &self.backend {
            PersistBackend::Postgres(store) => Ok(Some(
                store.search_candidates(request, limit, now, state).await?,
            )),
            PersistBackend::Memory | PersistBackend::JsonFile(_) => Ok(None),
        }
    }

    pub async fn write_with<R>(
        &self,
        f: impl FnOnce(&mut PersistedState) -> R,
    ) -> anyhow::Result<R> {
        let mut guard = self.inner.write().await;
        let previous = guard.clone();
        let result = f(&mut guard);
        let snapshot = guard.clone();
        drop(guard);
        persist_to_backend(&self.backend, &previous, &snapshot).await?;
        Ok(result)
    }
}

impl PostgresStore {
    async fn connect(database_url: &str) -> anyhow::Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await
            .with_context(|| "failed to connect to postgres")?;

        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .with_context(|| "failed to run postgres migrations")?;

        Ok(Self { pool })
    }

    async fn load_state(&self) -> anyhow::Result<PersistedState> {
        let mut state = PersistedState::default();

        for entry in sqlx::query_as::<_, EntryRow>(
            "SELECT id, kind, raw_text, asset_ref, captured_at, timezone, source_app, metadata, created_at FROM entries ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await
        .with_context(|| "failed to load entries")?
        {
            state.entries.insert(entry.id, entry.try_into()?);
        }

        for thread in sqlx::query_as::<_, ThreadRow>(
            "SELECT id, kind, title, summary, status, metadata, created_at, updated_at FROM threads ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await
        .with_context(|| "failed to load threads")?
        {
            state.threads.insert(thread.id, thread.try_into()?);
        }

        let artifact_embeddings = sqlx::query_as::<_, ArtifactEmbeddingRow>(
            "SELECT artifact_id, embedding_model, metadata, embedding FROM artifact_embeddings WHERE active = TRUE",
        )
        .fetch_all(&self.pool)
        .await
        .with_context(|| "failed to load artifact embeddings")?
        .into_iter()
        .map(|row| (row.artifact_id, row.into_embedding()))
        .collect::<BTreeMap<_, _>>();

        for artifact in sqlx::query_as::<_, ArtifactRow>(
            "SELECT id, entry_id, kind, ordinal, display_text, retrieval_text, metadata, created_at FROM artifacts ORDER BY entry_id, ordinal",
        )
        .fetch_all(&self.pool)
        .await
        .with_context(|| "failed to load artifacts")?
        {
            let id = artifact.id;
            let mut artifact: Artifact = artifact.try_into()?;
            artifact.embedding = artifact_embeddings.get(&id).cloned();
            state.artifacts.insert(id, artifact);
        }

        let memory_sources = sqlx::query_as::<_, MemorySourceRow>(
            "SELECT memory_id, artifact_id, evidence_rank FROM memory_sources ORDER BY memory_id, evidence_rank",
        )
        .fetch_all(&self.pool)
        .await
        .with_context(|| "failed to load memory sources")?
        .into_iter()
        .fold(BTreeMap::<Uuid, Vec<Uuid>>::new(), |mut acc, row| {
            acc.entry(row.memory_id).or_default().push(row.artifact_id);
            acc
        });

        let memory_embeddings = sqlx::query_as::<_, MemoryEmbeddingRow>(
            "SELECT memory_id, embedding_model, metadata, embedding FROM memory_embeddings WHERE active = TRUE",
        )
        .fetch_all(&self.pool)
        .await
        .with_context(|| "failed to load memory embeddings")?
        .into_iter()
        .map(|row| (row.memory_id, row.into_embedding()))
        .collect::<BTreeMap<_, _>>();

        for memory in sqlx::query_as::<_, MemoryRow>(
            "SELECT id, lineage_id, kind, subtype, display_text, retrieval_text, attrs, observed_at, valid_from, valid_to, confidence, salience, state, thread_id, parent_id, path, created_at, updated_at FROM memory_records ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await
        .with_context(|| "failed to load memory records")?
        {
            let id = memory.id;
            let mut memory: MemoryRecord = memory.try_into()?;
            memory.source_artifact_ids = memory_sources.get(&id).cloned().unwrap_or_default();
            memory.embedding = memory_embeddings.get(&id).cloned();
            state.memories.insert(id, memory);
        }

        state.profile_blocks.clear();
        for block in sqlx::query_as::<_, ProfileBlockRow>(
            "SELECT label, text, updated_at FROM profile_blocks ORDER BY label",
        )
        .fetch_all(&self.pool)
        .await
        .with_context(|| "failed to load profile blocks")?
        {
            let block: ProfileBlock = block.try_into()?;
            state.profile_blocks.insert(block.label, block);
        }
        if state.profile_blocks.is_empty() {
            state.profile_blocks = PersistedState::default().profile_blocks;
        }

        let trace_candidates = sqlx::query_as::<_, RetrievalTraceCandidateRow>(
            r#"
            SELECT
              trace_id,
              memory_id,
              lineage_id,
              COALESCE(semantic_score, 0) AS semantic_score,
              COALESCE(lexical_score, 0) AS lexical_score,
              fusion_score,
              temporal_bonus,
              thread_bonus,
              salience_bonus,
              confidence_bonus,
              reinjection_penalty,
              stale_penalty,
              final_score,
              candidate_rank,
              selected,
              injected_rank,
              created_at
            FROM retrieval_trace_candidates rtc
            JOIN memory_records mr ON mr.id = rtc.memory_id
            ORDER BY trace_id, candidate_rank
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .with_context(|| "failed to load retrieval trace candidates")?
        .into_iter()
        .fold(
            BTreeMap::<Uuid, Vec<RetrievalTraceCandidate>>::new(),
            |mut acc, row| {
                acc.entry(row.trace_id).or_default().push(row.into());
                acc
            },
        );

        for trace in sqlx::query_as::<_, RetrievalTraceRow>(
            "SELECT id, query_text, recent_context, active_thread_id, gate_decision, gate_confidence, gate_reason, final_context, created_at FROM retrieval_traces ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await
        .with_context(|| "failed to load retrieval traces")?
        {
            let id = trace.id;
            let candidates = trace_candidates.get(&id).cloned().unwrap_or_default();
            let mut trace: RetrievalTrace = trace.try_into()?;
            trace.selected_memory_ids = derive_selected_memory_ids(&candidates);
            trace.candidates = candidates;
            state.retrieval_traces.insert(id, trace);
        }

        Ok(state)
    }

    async fn search_candidates(
        &self,
        request: &AssembleContextRequest,
        limit: usize,
        now: DateTime<Utc>,
        state: &PersistedState,
    ) -> anyhow::Result<Vec<ScoredMemory>> {
        let focus_to = request.focus_from.map(|_| request.focus_to.unwrap_or(now));
        let query_embedding = request
            .query_embedding
            .as_ref()
            .map(|embedding| Vector::from(canonicalize_embedding_values(&embedding.values)));
        let candidate_limit = i32::try_from(limit.max(20)).context("search limit exceeds i32")?;
        let final_limit = i32::try_from(limit.max(1)).context("search limit exceeds i32")?;

        let rows = sqlx::query_as::<_, HybridCandidateRow>(HYBRID_MEMORY_CANDIDATES_SQL)
            .bind(build_query_material(request, &state.threads))
            .bind(query_embedding)
            .bind(now)
            .bind(request.focus_from)
            .bind(focus_to)
            .bind(request.active_thread_id)
            .bind(candidate_limit)
            .bind(candidate_limit)
            .bind(final_limit)
            .bind(PgInterval {
                months: 0,
                days: 7,
                microseconds: 0,
            })
            .fetch_all(&self.pool)
            .await
            .with_context(|| "failed to execute hybrid memory candidate query")?;

        let mut candidates = Vec::with_capacity(rows.len());
        for row in rows {
            let Some(memory) = state.memories.get(&row.id).cloned() else {
                continue;
            };
            candidates.push(ScoredMemory {
                memory,
                semantic_score: row.semantic_score.unwrap_or(0.0) as f32,
                lexical_score: row.lexical_score.unwrap_or(0.0) as f32,
                fusion_score: row.fusion_score as f32,
                temporal_bonus: row.temporal_bonus as f32,
                thread_bonus: row.thread_bonus as f32,
                salience_bonus: row.salience_bonus as f32,
                confidence_bonus: row.confidence_bonus as f32,
                reinjection_penalty: row.reinjection_penalty as f32,
                stale_penalty: row.stale_penalty as f32,
                final_score: row.final_score as f32,
                prior_injected: row.prior_injected,
                candidate_rank: usize::try_from(row.candidate_rank).unwrap_or_default(),
            });
        }

        Ok(candidates)
    }

    async fn persist_delta(
        &self,
        previous: &PersistedState,
        current: &PersistedState,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        persist_entries_delta(&mut tx, previous, current).await?;
        persist_threads_delta(&mut tx, previous, current).await?;
        persist_artifacts_delta(&mut tx, previous, current).await?;
        persist_memories_delta(&mut tx, previous, current).await?;
        persist_profile_blocks_delta(&mut tx, previous, current).await?;
        persist_traces_delta(&mut tx, previous, current).await?;
        tx.commit().await?;
        Ok(())
    }
}

async fn load_state_from_backend(backend: &PersistBackend) -> anyhow::Result<PersistedState> {
    match backend {
        PersistBackend::Memory => Ok(PersistedState::default()),
        PersistBackend::JsonFile(path) => load_state_from_file(path).await,
        PersistBackend::Postgres(store) => store.load_state().await,
    }
}

async fn persist_to_backend(
    backend: &PersistBackend,
    previous: &PersistedState,
    snapshot: &PersistedState,
) -> anyhow::Result<()> {
    match backend {
        PersistBackend::Memory => Ok(()),
        PersistBackend::JsonFile(path) => persist_to_file(path, snapshot).await,
        PersistBackend::Postgres(store) => store.persist_delta(previous, snapshot).await,
    }
}

async fn load_state_from_file(path: &Path) -> anyhow::Result<PersistedState> {
    if path.exists() {
        let raw = fs::read_to_string(path)
            .await
            .with_context(|| format!("failed to read snapshot at {}", path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse snapshot at {}", path.display()))
    } else {
        Ok(PersistedState::default())
    }
}

async fn persist_to_file(path: &Path, snapshot: &PersistedState) -> anyhow::Result<()> {
    ensure_parent_dir(path).await?;
    let body = serde_json::to_string_pretty(snapshot).context("failed to serialize state")?;
    fs::write(path, body)
        .await
        .with_context(|| format!("failed to write snapshot to {}", path.display()))?;
    Ok(())
}

async fn ensure_parent_dir(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    Ok(())
}

const DEFAULT_ARTIFACT_EMBEDDING_MODEL: &str = "perplexity-ai/pplx-embed-context-v1-0.6b";
const DEFAULT_MEMORY_EMBEDDING_MODEL: &str = "perplexity-ai/pplx-embed-v1-0.6b";
const DEFAULT_EMBEDDING_VERSION: &str = "client-prepared-v1";
const DEFAULT_QUERY_EMBEDDING_MODEL: &str = "client_or_placeholder";

async fn persist_entries_delta(
    tx: &mut Transaction<'_, Postgres>,
    previous: &PersistedState,
    current: &PersistedState,
) -> anyhow::Result<()> {
    for (id, entry) in &current.entries {
        match previous.entries.get(id) {
            Some(existing) if existing != entry => {
                bail!("entry {id} changed after creation; entries are append-only")
            }
            Some(_) => {}
            None => insert_entry(tx, entry).await?,
        }
    }

    for id in previous.entries.keys() {
        if !current.entries.contains_key(id) {
            delete_entry(tx, *id).await?;
        }
    }

    Ok(())
}

async fn persist_threads_delta(
    tx: &mut Transaction<'_, Postgres>,
    previous: &PersistedState,
    current: &PersistedState,
) -> anyhow::Result<()> {
    for (id, thread) in &current.threads {
        if previous.threads.get(id) != Some(thread) {
            upsert_thread(tx, thread).await?;
        }
    }

    for id in previous.threads.keys() {
        if !current.threads.contains_key(id) {
            delete_thread(tx, *id).await?;
        }
    }

    Ok(())
}

async fn persist_artifacts_delta(
    tx: &mut Transaction<'_, Postgres>,
    previous: &PersistedState,
    current: &PersistedState,
) -> anyhow::Result<()> {
    for (id, artifact) in &current.artifacts {
        if previous.artifacts.get(id) != Some(artifact) {
            upsert_artifact(tx, artifact).await?;
            replace_artifact_embedding(tx, artifact).await?;
        }
    }

    for id in previous.artifacts.keys() {
        if !current.artifacts.contains_key(id) {
            delete_artifact(tx, *id).await?;
        }
    }

    Ok(())
}

async fn persist_memories_delta(
    tx: &mut Transaction<'_, Postgres>,
    previous: &PersistedState,
    current: &PersistedState,
) -> anyhow::Result<()> {
    for (id, memory) in &current.memories {
        if previous.memories.get(id) != Some(memory) {
            upsert_memory(tx, memory).await?;
            replace_memory_sources(tx, memory).await?;
            replace_memory_embedding(tx, memory).await?;
        }
    }

    for id in previous.memories.keys() {
        if !current.memories.contains_key(id) {
            delete_memory(tx, *id).await?;
        }
    }

    Ok(())
}

async fn persist_profile_blocks_delta(
    tx: &mut Transaction<'_, Postgres>,
    previous: &PersistedState,
    current: &PersistedState,
) -> anyhow::Result<()> {
    for (label, block) in &current.profile_blocks {
        if previous.profile_blocks.get(label) != Some(block) {
            upsert_profile_block(tx, block).await?;
        }
    }

    for label in previous.profile_blocks.keys() {
        if !current.profile_blocks.contains_key(label) {
            delete_profile_block(tx, *label).await?;
        }
    }

    Ok(())
}

async fn persist_traces_delta(
    tx: &mut Transaction<'_, Postgres>,
    previous: &PersistedState,
    current: &PersistedState,
) -> anyhow::Result<()> {
    for (id, trace) in &current.retrieval_traces {
        if previous.retrieval_traces.get(id) != Some(trace) {
            upsert_trace(tx, trace).await?;
            replace_trace_candidates(tx, trace).await?;
        }
    }

    for id in previous.retrieval_traces.keys() {
        if !current.retrieval_traces.contains_key(id) {
            delete_trace(tx, *id).await?;
        }
    }

    Ok(())
}

async fn insert_entry(tx: &mut Transaction<'_, Postgres>, entry: &Entry) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO entries (
          id,
          kind,
          raw_text,
          asset_ref,
          captured_at,
          timezone,
          source_app,
          metadata,
          created_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(entry.id)
    .bind(enum_text(&entry.kind)?)
    .bind(&entry.raw_text)
    .bind(&entry.asset_ref)
    .bind(entry.captured_at)
    .bind(&entry.timezone)
    .bind(&entry.source_app)
    .bind(Json(entry.metadata.clone()))
    .bind(entry.created_at)
    .execute(tx.as_mut())
    .await
    .with_context(|| format!("failed to insert entry {}", entry.id))?;
    Ok(())
}

async fn delete_entry(tx: &mut Transaction<'_, Postgres>, entry_id: Uuid) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM entries WHERE id = $1")
        .bind(entry_id)
        .execute(tx.as_mut())
        .await
        .with_context(|| format!("failed to delete entry {entry_id}"))?;
    Ok(())
}

async fn upsert_thread(tx: &mut Transaction<'_, Postgres>, thread: &Thread) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO threads (
          id,
          kind,
          title,
          summary,
          status,
          metadata,
          created_at,
          updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ON CONFLICT (id) DO UPDATE SET
          kind = EXCLUDED.kind,
          title = EXCLUDED.title,
          summary = EXCLUDED.summary,
          status = EXCLUDED.status,
          metadata = EXCLUDED.metadata,
          updated_at = EXCLUDED.updated_at
        "#,
    )
    .bind(thread.id)
    .bind(enum_text(&thread.kind)?)
    .bind(&thread.title)
    .bind(&thread.summary)
    .bind(enum_text(&thread.status)?)
    .bind(Json(thread.metadata.clone()))
    .bind(thread.created_at)
    .bind(thread.updated_at)
    .execute(tx.as_mut())
    .await
    .with_context(|| format!("failed to upsert thread {}", thread.id))?;
    Ok(())
}

async fn delete_thread(tx: &mut Transaction<'_, Postgres>, thread_id: Uuid) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM threads WHERE id = $1")
        .bind(thread_id)
        .execute(tx.as_mut())
        .await
        .with_context(|| format!("failed to delete thread {thread_id}"))?;
    Ok(())
}

async fn upsert_artifact(
    tx: &mut Transaction<'_, Postgres>,
    artifact: &Artifact,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO artifacts (
          id,
          entry_id,
          kind,
          ordinal,
          display_text,
          retrieval_text,
          metadata,
          created_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ON CONFLICT (id) DO UPDATE SET
          entry_id = EXCLUDED.entry_id,
          kind = EXCLUDED.kind,
          ordinal = EXCLUDED.ordinal,
          display_text = EXCLUDED.display_text,
          retrieval_text = EXCLUDED.retrieval_text,
          metadata = EXCLUDED.metadata
        "#,
    )
    .bind(artifact.id)
    .bind(artifact.entry_id)
    .bind(enum_text(&artifact.kind)?)
    .bind(i32::try_from(artifact.ordinal).context("artifact ordinal exceeds i32")?)
    .bind(&artifact.display_text)
    .bind(&artifact.retrieval_text)
    .bind(Json(artifact.metadata.clone()))
    .bind(artifact.created_at)
    .execute(tx.as_mut())
    .await
    .with_context(|| format!("failed to upsert artifact {}", artifact.id))?;
    Ok(())
}

async fn replace_artifact_embedding(
    tx: &mut Transaction<'_, Postgres>,
    artifact: &Artifact,
) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM artifact_embeddings WHERE artifact_id = $1")
        .bind(artifact.id)
        .execute(tx.as_mut())
        .await
        .with_context(|| format!("failed to clear artifact embeddings for {}", artifact.id))?;

    let Some(embedding) = &artifact.embedding else {
        return Ok(());
    };

    sqlx::query(
        r#"
        INSERT INTO artifact_embeddings (
          artifact_id,
          embedding_model,
          embedding_version,
          metadata,
          quantization,
          normalized,
          dims,
          embedding,
          active,
          created_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, TRUE, $9)
        "#,
    )
    .bind(artifact.id)
    .bind(
        embedding
            .model
            .clone()
            .unwrap_or_else(|| DEFAULT_ARTIFACT_EMBEDDING_MODEL.to_string()),
    )
    .bind(DEFAULT_EMBEDDING_VERSION)
    .bind(Json(embedding_metadata(embedding)))
    .bind("float32")
    .bind(false)
    .bind(1024_i32)
    .bind(Vector::from(canonicalize_embedding_values(
        &embedding.values,
    )))
    .bind(artifact.created_at)
    .execute(tx.as_mut())
    .await
    .with_context(|| format!("failed to insert artifact embedding for {}", artifact.id))?;
    Ok(())
}

async fn delete_artifact(
    tx: &mut Transaction<'_, Postgres>,
    artifact_id: Uuid,
) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM artifacts WHERE id = $1")
        .bind(artifact_id)
        .execute(tx.as_mut())
        .await
        .with_context(|| format!("failed to delete artifact {artifact_id}"))?;
    Ok(())
}

async fn upsert_memory(
    tx: &mut Transaction<'_, Postgres>,
    memory: &MemoryRecord,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO memory_records (
          id,
          lineage_id,
          kind,
          subtype,
          display_text,
          retrieval_text,
          attrs,
          observed_at,
          valid_from,
          valid_to,
          confidence,
          salience,
          state,
          thread_id,
          parent_id,
          supersedes_id,
          path,
          created_at,
          updated_at
        )
        VALUES (
          $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
          $11, $12, $13, $14, $15, NULL, $16, $17, $18
        )
        ON CONFLICT (id) DO UPDATE SET
          lineage_id = EXCLUDED.lineage_id,
          kind = EXCLUDED.kind,
          subtype = EXCLUDED.subtype,
          display_text = EXCLUDED.display_text,
          retrieval_text = EXCLUDED.retrieval_text,
          attrs = EXCLUDED.attrs,
          observed_at = EXCLUDED.observed_at,
          valid_from = EXCLUDED.valid_from,
          valid_to = EXCLUDED.valid_to,
          confidence = EXCLUDED.confidence,
          salience = EXCLUDED.salience,
          state = EXCLUDED.state,
          thread_id = EXCLUDED.thread_id,
          parent_id = EXCLUDED.parent_id,
          path = EXCLUDED.path,
          updated_at = EXCLUDED.updated_at
        "#,
    )
    .bind(memory.id)
    .bind(memory.lineage_id)
    .bind(enum_text(&memory.kind)?)
    .bind(enum_text(&memory.subtype)?)
    .bind(&memory.display_text)
    .bind(&memory.retrieval_text)
    .bind(Json(memory.attrs.clone()))
    .bind(memory.observed_at)
    .bind(memory.valid_from)
    .bind(memory.valid_to)
    .bind(memory.confidence as f64)
    .bind(memory.salience as f64)
    .bind(enum_text(&memory.state)?)
    .bind(memory.thread_id)
    .bind(memory.parent_id)
    .bind(&memory.path)
    .bind(memory.created_at)
    .bind(memory.updated_at)
    .execute(tx.as_mut())
    .await
    .with_context(|| format!("failed to upsert memory {}", memory.id))?;
    Ok(())
}

async fn replace_memory_sources(
    tx: &mut Transaction<'_, Postgres>,
    memory: &MemoryRecord,
) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM memory_sources WHERE memory_id = $1")
        .bind(memory.id)
        .execute(tx.as_mut())
        .await
        .with_context(|| format!("failed to clear memory sources for {}", memory.id))?;

    for (rank, artifact_id) in memory.source_artifact_ids.iter().enumerate() {
        sqlx::query(
            r#"
            INSERT INTO memory_sources (
              memory_id,
              artifact_id,
              evidence_rank,
              metadata,
              created_at
            )
            VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(memory.id)
        .bind(*artifact_id)
        .bind(i16::try_from(rank).context("memory evidence rank exceeds i16")?)
        .bind(Json(json!({})))
        .bind(memory.created_at)
        .execute(tx.as_mut())
        .await
        .with_context(|| {
            format!(
                "failed to insert memory source {} -> {}",
                memory.id, artifact_id
            )
        })?;
    }

    Ok(())
}

async fn replace_memory_embedding(
    tx: &mut Transaction<'_, Postgres>,
    memory: &MemoryRecord,
) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM memory_embeddings WHERE memory_id = $1")
        .bind(memory.id)
        .execute(tx.as_mut())
        .await
        .with_context(|| format!("failed to clear memory embeddings for {}", memory.id))?;

    let Some(embedding) = &memory.embedding else {
        return Ok(());
    };

    sqlx::query(
        r#"
        INSERT INTO memory_embeddings (
          memory_id,
          embedding_model,
          embedding_version,
          metadata,
          quantization,
          normalized,
          dims,
          embedding,
          active,
          created_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, TRUE, $9)
        "#,
    )
    .bind(memory.id)
    .bind(
        embedding
            .model
            .clone()
            .unwrap_or_else(|| DEFAULT_MEMORY_EMBEDDING_MODEL.to_string()),
    )
    .bind(DEFAULT_EMBEDDING_VERSION)
    .bind(Json(embedding_metadata(embedding)))
    .bind("float32")
    .bind(false)
    .bind(1024_i32)
    .bind(Vector::from(canonicalize_embedding_values(
        &embedding.values,
    )))
    .bind(memory.created_at)
    .execute(tx.as_mut())
    .await
    .with_context(|| format!("failed to insert memory embedding for {}", memory.id))?;
    Ok(())
}

async fn delete_memory(tx: &mut Transaction<'_, Postgres>, memory_id: Uuid) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM memory_records WHERE id = $1")
        .bind(memory_id)
        .execute(tx.as_mut())
        .await
        .with_context(|| format!("failed to delete memory {memory_id}"))?;
    Ok(())
}

async fn upsert_profile_block(
    tx: &mut Transaction<'_, Postgres>,
    block: &ProfileBlock,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO profile_blocks (label, text, metadata, updated_at)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (label) DO UPDATE SET
          text = EXCLUDED.text,
          metadata = EXCLUDED.metadata,
          updated_at = EXCLUDED.updated_at
        "#,
    )
    .bind(enum_text(&block.label)?)
    .bind(&block.text)
    .bind(Json(json!({})))
    .bind(block.updated_at)
    .execute(tx.as_mut())
    .await
    .with_context(|| format!("failed to upsert profile block {:?}", block.label))?;
    Ok(())
}

async fn delete_profile_block(
    tx: &mut Transaction<'_, Postgres>,
    label: crate::model::ProfileLabel,
) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM profile_blocks WHERE label = $1")
        .bind(enum_text(&label)?)
        .execute(tx.as_mut())
        .await
        .with_context(|| format!("failed to delete profile block {:?}", label))?;
    Ok(())
}

async fn upsert_trace(
    tx: &mut Transaction<'_, Postgres>,
    trace: &RetrievalTrace,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO retrieval_traces (
          id,
          query_text,
          recent_context,
          active_thread_id,
          query_embedding_model,
          gate_decision,
          gate_confidence,
          gate_reason,
          final_context,
          request_metadata,
          created_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
        ON CONFLICT (id) DO UPDATE SET
          query_text = EXCLUDED.query_text,
          recent_context = EXCLUDED.recent_context,
          active_thread_id = EXCLUDED.active_thread_id,
          query_embedding_model = EXCLUDED.query_embedding_model,
          gate_decision = EXCLUDED.gate_decision,
          gate_confidence = EXCLUDED.gate_confidence,
          gate_reason = EXCLUDED.gate_reason,
          final_context = EXCLUDED.final_context,
          request_metadata = EXCLUDED.request_metadata
        "#,
    )
    .bind(trace.id)
    .bind(&trace.query)
    .bind(&trace.recent_context)
    .bind(trace.active_thread_id)
    .bind(DEFAULT_QUERY_EMBEDDING_MODEL)
    .bind(enum_text(&trace.gate_decision)?)
    .bind(trace.gate_confidence as f64)
    .bind(&trace.gate_reason)
    .bind(&trace.final_context)
    .bind(Json(json!({})))
    .bind(trace.created_at)
    .execute(tx.as_mut())
    .await
    .with_context(|| format!("failed to upsert retrieval trace {}", trace.id))?;
    Ok(())
}

async fn replace_trace_candidates(
    tx: &mut Transaction<'_, Postgres>,
    trace: &RetrievalTrace,
) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM retrieval_trace_candidates WHERE trace_id = $1")
        .bind(trace.id)
        .execute(tx.as_mut())
        .await
        .with_context(|| {
            format!(
                "failed to clear retrieval trace candidates for {}",
                trace.id
            )
        })?;

    for candidate in &trace.candidates {
        sqlx::query(
            r#"
            INSERT INTO retrieval_trace_candidates (
              trace_id,
              memory_id,
              candidate_rank,
              semantic_rank,
              lexical_rank,
              semantic_score,
              lexical_score,
              fusion_score,
              temporal_bonus,
              thread_bonus,
              salience_bonus,
              confidence_bonus,
              reinjection_penalty,
              stale_penalty,
              final_score,
              gate_label,
              gate_score,
              selected,
              injected_rank,
              created_at
            )
            VALUES (
              $1, $2, $3, NULL, NULL, $4, $5, $6, $7, $8, $9,
              $10, $11, $12, $13, NULL, NULL, $14, $15, $16
            )
            "#,
        )
        .bind(trace.id)
        .bind(candidate.memory_id)
        .bind(i32::try_from(candidate.candidate_rank).context("candidate rank exceeds i32")?)
        .bind(candidate.semantic_score as f64)
        .bind(candidate.lexical_score as f64)
        .bind(candidate.fusion_score as f64)
        .bind(candidate.temporal_bonus as f64)
        .bind(candidate.thread_bonus as f64)
        .bind(candidate.salience_bonus as f64)
        .bind(candidate.confidence_bonus as f64)
        .bind(candidate.reinjection_penalty as f64)
        .bind(candidate.stale_penalty as f64)
        .bind(candidate.final_score as f64)
        .bind(candidate.selected)
        .bind(
            candidate
                .injected_rank
                .map(|rank| i32::try_from(rank))
                .transpose()
                .context("injected rank exceeds i32")?,
        )
        .bind(trace.created_at)
        .execute(tx.as_mut())
        .await
        .with_context(|| {
            format!(
                "failed to insert retrieval trace candidate {} -> {}",
                trace.id, candidate.memory_id
            )
        })?;
    }

    Ok(())
}

async fn delete_trace(tx: &mut Transaction<'_, Postgres>, trace_id: Uuid) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM retrieval_traces WHERE id = $1")
        .bind(trace_id)
        .execute(tx.as_mut())
        .await
        .with_context(|| format!("failed to delete retrieval trace {trace_id}"))?;
    Ok(())
}

fn embedding_metadata(embedding: &EmbeddingVector) -> serde_json::Value {
    json!({
        "device": embedding.device,
        "source": embedding.source,
    })
}

fn enum_text<T: Serialize>(value: &T) -> anyhow::Result<String> {
    serde_json::to_string(value)
        .map(|text| text.trim_matches('"').to_string())
        .context("failed to serialize enum")
}

fn parse_enum<T: DeserializeOwned>(value: &str) -> anyhow::Result<T> {
    serde_json::from_str(&format!("\"{value}\"")).context("failed to parse enum")
}

fn canonicalize_embedding_values(values: &[f32]) -> Vec<f32> {
    let mut normalized = values.to_vec();
    if normalized.len() > 1024 {
        normalized.truncate(1024);
    } else if normalized.len() < 1024 {
        normalized.resize(1024, 0.0);
    }
    normalized
}

fn derive_selected_memory_ids(candidates: &[RetrievalTraceCandidate]) -> Vec<Uuid> {
    let mut selected = candidates
        .iter()
        .filter(|candidate| candidate.selected)
        .collect::<Vec<_>>();
    selected.sort_by_key(|candidate| candidate.injected_rank.unwrap_or(candidate.candidate_rank));
    selected
        .into_iter()
        .map(|candidate| candidate.memory_id)
        .collect()
}

#[derive(FromRow)]
struct HybridCandidateRow {
    id: Uuid,
    semantic_score: Option<f64>,
    lexical_score: Option<f64>,
    fusion_score: f64,
    temporal_bonus: f64,
    thread_bonus: f64,
    salience_bonus: f64,
    confidence_bonus: f64,
    reinjection_penalty: f64,
    stale_penalty: f64,
    final_score: f64,
    prior_injected: bool,
    candidate_rank: i64,
}

#[derive(FromRow)]
struct EntryRow {
    id: Uuid,
    kind: String,
    raw_text: Option<String>,
    asset_ref: Option<String>,
    captured_at: DateTime<Utc>,
    timezone: String,
    source_app: Option<String>,
    metadata: Json<serde_json::Value>,
    created_at: DateTime<Utc>,
}

impl TryFrom<EntryRow> for Entry {
    type Error = anyhow::Error;

    fn try_from(row: EntryRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            kind: parse_enum(&row.kind)?,
            raw_text: row.raw_text,
            asset_ref: row.asset_ref,
            captured_at: row.captured_at,
            timezone: row.timezone,
            source_app: row.source_app,
            metadata: row.metadata.0,
            created_at: row.created_at,
        })
    }
}

#[derive(FromRow)]
struct ArtifactRow {
    id: Uuid,
    entry_id: Uuid,
    kind: String,
    ordinal: i32,
    display_text: String,
    retrieval_text: String,
    metadata: Json<serde_json::Value>,
    created_at: DateTime<Utc>,
}

impl TryFrom<ArtifactRow> for Artifact {
    type Error = anyhow::Error;

    fn try_from(row: ArtifactRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            entry_id: row.entry_id,
            kind: parse_enum(&row.kind)?,
            ordinal: u32::try_from(row.ordinal).unwrap_or_default(),
            display_text: row.display_text,
            retrieval_text: row.retrieval_text,
            embedding: None,
            metadata: row.metadata.0,
            created_at: row.created_at,
        })
    }
}

#[derive(FromRow)]
struct ArtifactEmbeddingRow {
    artifact_id: Uuid,
    embedding_model: String,
    metadata: Option<Json<serde_json::Value>>,
    embedding: Vector,
}

impl ArtifactEmbeddingRow {
    fn into_embedding(self) -> EmbeddingVector {
        let metadata = self.metadata.map(|json| json.0).unwrap_or_default();
        EmbeddingVector {
            values: self.embedding.to_vec(),
            model: Some(self.embedding_model),
            device: metadata
                .get("device")
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned),
            source: metadata
                .get("source")
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned),
        }
    }
}

#[derive(FromRow)]
struct ThreadRow {
    id: Uuid,
    kind: String,
    title: String,
    summary: String,
    status: String,
    metadata: Json<serde_json::Value>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<ThreadRow> for Thread {
    type Error = anyhow::Error;

    fn try_from(row: ThreadRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            kind: parse_enum(&row.kind)?,
            title: row.title,
            summary: row.summary,
            status: parse_enum(&row.status)?,
            metadata: row.metadata.0,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(FromRow)]
struct MemoryRow {
    id: Uuid,
    lineage_id: Uuid,
    kind: String,
    subtype: String,
    display_text: String,
    retrieval_text: String,
    attrs: Json<serde_json::Value>,
    observed_at: Option<DateTime<Utc>>,
    valid_from: DateTime<Utc>,
    valid_to: Option<DateTime<Utc>>,
    confidence: f64,
    salience: f64,
    state: String,
    thread_id: Option<Uuid>,
    parent_id: Option<Uuid>,
    path: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<MemoryRow> for MemoryRecord {
    type Error = anyhow::Error;

    fn try_from(row: MemoryRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            lineage_id: row.lineage_id,
            kind: parse_enum(&row.kind)?,
            subtype: parse_enum(&row.subtype)?,
            display_text: row.display_text,
            retrieval_text: row.retrieval_text,
            attrs: row.attrs.0,
            observed_at: row.observed_at,
            valid_from: row.valid_from,
            valid_to: row.valid_to,
            confidence: row.confidence as f32,
            salience: row.salience as f32,
            state: parse_enum(&row.state)?,
            embedding: None,
            source_artifact_ids: Vec::new(),
            thread_id: row.thread_id,
            parent_id: row.parent_id,
            path: row.path,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(FromRow)]
struct MemoryEmbeddingRow {
    memory_id: Uuid,
    embedding_model: String,
    metadata: Option<Json<serde_json::Value>>,
    embedding: Vector,
}

impl MemoryEmbeddingRow {
    fn into_embedding(self) -> EmbeddingVector {
        let metadata = self.metadata.map(|json| json.0).unwrap_or_default();
        EmbeddingVector {
            values: self.embedding.to_vec(),
            model: Some(self.embedding_model),
            device: metadata
                .get("device")
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned),
            source: metadata
                .get("source")
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned),
        }
    }
}

#[derive(FromRow)]
struct MemorySourceRow {
    memory_id: Uuid,
    artifact_id: Uuid,
    _evidence_rank: i16,
}

#[derive(FromRow)]
struct ProfileBlockRow {
    label: String,
    text: String,
    updated_at: DateTime<Utc>,
}

impl TryFrom<ProfileBlockRow> for ProfileBlock {
    type Error = anyhow::Error;

    fn try_from(row: ProfileBlockRow) -> Result<Self, Self::Error> {
        Ok(Self {
            label: parse_enum(&row.label)?,
            text: row.text,
            updated_at: row.updated_at,
        })
    }
}

#[derive(FromRow)]
struct RetrievalTraceRow {
    id: Uuid,
    query_text: String,
    recent_context: Option<String>,
    active_thread_id: Option<Uuid>,
    gate_decision: Option<String>,
    gate_confidence: Option<f64>,
    gate_reason: Option<String>,
    final_context: Option<String>,
    created_at: DateTime<Utc>,
}

impl TryFrom<RetrievalTraceRow> for RetrievalTrace {
    type Error = anyhow::Error;

    fn try_from(row: RetrievalTraceRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            query: row.query_text,
            recent_context: row.recent_context,
            active_thread_id: row.active_thread_id,
            gate_decision: row
                .gate_decision
                .as_deref()
                .map(parse_enum)
                .transpose()?
                .unwrap_or(GateDecision::NoInject),
            gate_confidence: row.gate_confidence.unwrap_or(0.0) as f32,
            gate_reason: row.gate_reason.unwrap_or_default(),
            final_context: row.final_context,
            candidates: Vec::new(),
            selected_memory_ids: Vec::new(),
            created_at: row.created_at,
        })
    }
}

#[derive(FromRow)]
struct RetrievalTraceCandidateRow {
    trace_id: Uuid,
    memory_id: Uuid,
    lineage_id: Uuid,
    semantic_score: f64,
    lexical_score: f64,
    fusion_score: f64,
    temporal_bonus: f64,
    thread_bonus: f64,
    salience_bonus: f64,
    confidence_bonus: f64,
    reinjection_penalty: f64,
    stale_penalty: f64,
    final_score: f64,
    candidate_rank: i32,
    selected: bool,
    injected_rank: Option<i32>,
    created_at: DateTime<Utc>,
}

impl From<RetrievalTraceCandidateRow> for RetrievalTraceCandidate {
    fn from(row: RetrievalTraceCandidateRow) -> Self {
        let _ = row.created_at;
        Self {
            memory_id: row.memory_id,
            lineage_id: row.lineage_id,
            semantic_score: row.semantic_score as f32,
            lexical_score: row.lexical_score as f32,
            fusion_score: row.fusion_score as f32,
            temporal_bonus: row.temporal_bonus as f32,
            thread_bonus: row.thread_bonus as f32,
            salience_bonus: row.salience_bonus as f32,
            confidence_bonus: row.confidence_bonus as f32,
            reinjection_penalty: row.reinjection_penalty as f32,
            stale_penalty: row.stale_penalty as f32,
            final_score: row.final_score as f32,
            candidate_rank: usize::try_from(row.candidate_rank).unwrap_or_default(),
            selected: row.selected,
            injected_rank: row
                .injected_rank
                .and_then(|rank| usize::try_from(rank).ok()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use tempfile::tempdir;

    use crate::model::ProfileLabel;

    use super::*;

    #[tokio::test]
    async fn file_backend_round_trip_persists_state() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let store = SharedStore::load(Some(path.clone()), None).await.unwrap();
        assert_eq!(store.backend_kind(), BackendKind::JsonFile);

        store
            .write_with(|state| {
                let entry = crate::model::Entry {
                    id: uuid::Uuid::new_v4(),
                    kind: crate::model::EntryKind::TextJournal,
                    raw_text: Some("hello".to_string()),
                    asset_ref: None,
                    captured_at: crate::model::now_utc(),
                    timezone: "UTC".to_string(),
                    source_app: None,
                    metadata: crate::model::empty_object(),
                    created_at: crate::model::now_utc(),
                };
                state.entries.insert(entry.id, entry);
            })
            .await
            .unwrap();

        let reloaded = SharedStore::load(Some(path), None).await.unwrap();
        assert_eq!(reloaded.read_clone().await.entries.len(), 1);
    }

    #[test]
    fn in_memory_backend_reports_kind() {
        let store = SharedStore::new_in_memory();
        assert_eq!(store.backend_kind(), BackendKind::Memory);
    }

    #[tokio::test]
    async fn postgres_backend_round_trip_when_test_database_url_is_present() {
        let Some(database_url) = env::var("TEST_DATABASE_URL").ok() else {
            return;
        };

        let store = match SharedStore::load(None, Some(database_url.clone())).await {
            Ok(store) => store,
            Err(error) if error.to_string().contains("vector") => return,
            Err(error) => panic!("{error}"),
        };
        assert_eq!(store.backend_kind(), BackendKind::Postgres);

        store
            .write_with(|state| {
                state
                    .profile_blocks
                    .get_mut(&ProfileLabel::Identity)
                    .unwrap()
                    .text = "postgres-backed".to_string();
            })
            .await
            .unwrap();

        let reloaded = match SharedStore::load(None, Some(database_url.clone())).await {
            Ok(store) => store,
            Err(error) if error.to_string().contains("vector") => return,
            Err(error) => panic!("{error}"),
        };
        assert_eq!(
            reloaded
                .read_clone()
                .await
                .profile_blocks
                .get(&ProfileLabel::Identity)
                .unwrap()
                .text,
            "postgres-backed"
        );

        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM profile_blocks")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(count > 0);

        let identity_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM profile_blocks WHERE label = 'identity'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(identity_count, 1);
    }
}
