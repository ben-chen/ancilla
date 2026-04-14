use std::collections::{BTreeMap, BTreeSet, HashSet};

use chrono::{DateTime, Duration, Utc};

use crate::model::{
    AssembleContextRequest, GateDecision, MemoryKind, MemoryRecord, MemoryState, ProfileBlock,
    ProfileLabel, RetrievalTrace, RetrievalTraceCandidate, ScoredMemory, Thread,
};

const EMBEDDING_DIMS: usize = 1024;
const RRF_K: f32 = 60.0;
const GATE_THRESHOLD: f32 = 0.18;

#[derive(Clone, Debug)]
pub struct SearchEnvironment<'a> {
    pub memories: &'a BTreeMap<uuid::Uuid, MemoryRecord>,
    pub threads: &'a BTreeMap<uuid::Uuid, Thread>,
    pub retrieval_traces: &'a BTreeMap<uuid::Uuid, RetrievalTrace>,
}

#[derive(Clone, Debug)]
pub struct GateResult {
    pub decision: GateDecision,
    pub confidence: f32,
    pub reason: String,
    pub selected: Vec<ScoredMemory>,
}

pub fn build_query_material(
    request: &AssembleContextRequest,
    threads: &BTreeMap<uuid::Uuid, Thread>,
) -> String {
    let thread_hint = request
        .active_thread_id
        .and_then(|id| threads.get(&id))
        .map(|thread| format!("{} {}", thread.title, thread.summary))
        .unwrap_or_default();
    let recent_turns = request
        .recent_turns
        .iter()
        .map(|turn| turn.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");

    format!(
        "{} {} {} {}",
        request.query,
        request.recent_context.clone().unwrap_or_default(),
        recent_turns,
        thread_hint
    )
    .trim()
    .to_string()
}

pub fn score_memories(
    env: SearchEnvironment<'_>,
    request: &AssembleContextRequest,
    limit: usize,
    now: DateTime<Utc>,
) -> Vec<ScoredMemory> {
    let query_material = build_query_material(request, env.threads);

    let mut eligible = env
        .memories
        .values()
        .filter(|memory| memory.state == MemoryState::Accepted)
        .filter(|memory| {
            if let Some(active_kind) = request.active_thread_id {
                if memory.thread_id == Some(active_kind) {
                    return true;
                }
            }

            if let Some(focus_from) = request.focus_from {
                let focus_to = request.focus_to.unwrap_or(now);
                let valid_overlap = overlaps(
                    memory.valid_from,
                    memory.valid_to.unwrap_or(DateTime::<Utc>::MAX_UTC),
                    focus_from,
                    focus_to,
                );
                let observed_overlap = memory
                    .observed_at
                    .map(|observed| observed >= focus_from && observed <= focus_to)
                    .unwrap_or(false);
                valid_overlap || observed_overlap
            } else {
                memory.kind == MemoryKind::Episodic
                    || memory
                        .valid_to
                        .map(|valid_to| valid_to > now)
                        .unwrap_or(true)
            }
        })
        .cloned()
        .collect::<Vec<_>>();

    if let Some(kind) = request.active_thread_id {
        eligible.sort_by_key(|memory| memory.thread_id != Some(kind));
    }

    let lexical_scores = lexical_rankings(&query_material, &eligible);
    let semantic_scores =
        semantic_rankings(&query_material, request.query_embedding.as_ref(), &eligible);

    let lexical_map = lexical_scores
        .iter()
        .enumerate()
        .map(|(rank, (id, score))| (*id, (rank + 1, *score)))
        .collect::<BTreeMap<_, _>>();
    let semantic_map = semantic_scores
        .iter()
        .enumerate()
        .map(|(rank, (id, score))| (*id, (rank + 1, *score)))
        .collect::<BTreeMap<_, _>>();

    let mut candidate_ids = HashSet::new();
    let top_lexical = lexical_scores.iter().take(limit).map(|(id, _)| *id);
    let top_semantic = semantic_scores.iter().take(limit).map(|(id, _)| *id);
    candidate_ids.extend(top_lexical);
    candidate_ids.extend(top_semantic);

    let mut results = eligible
        .into_iter()
        .filter(|memory| candidate_ids.contains(&memory.id))
        .map(|memory| {
            let (semantic_rank, semantic_score) = semantic_map
                .get(&memory.id)
                .copied()
                .unwrap_or((usize::MAX, 0.0));
            let (lexical_rank, lexical_score) = lexical_map
                .get(&memory.id)
                .copied()
                .unwrap_or((usize::MAX, 0.0));
            let fusion_score = rrf(semantic_rank) + rrf(lexical_rank);
            let temporal_bonus = temporal_bonus(&memory, request, now);
            let thread_bonus = if memory.thread_id == request.active_thread_id {
                0.10
            } else {
                0.0
            };
            let salience_bonus = (memory.salience * 0.12).min(0.12);
            let confidence_bonus = (memory.confidence * 0.10).min(0.10);
            let reinjection_penalty = reinjection_penalty(memory.id, env.retrieval_traces, now);
            let stale_penalty = stale_penalty(&memory, request, now);
            let final_score =
                fusion_score + temporal_bonus + thread_bonus + salience_bonus + confidence_bonus
                    - reinjection_penalty
                    - stale_penalty;
            let prior_injected = env.retrieval_traces.values().any(|trace| {
                trace.selected_memory_ids.contains(&memory.id)
                    && trace.created_at >= now - Duration::days(7)
            });

            ScoredMemory {
                memory,
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
                prior_injected,
                candidate_rank: 0,
            }
        })
        .collect::<Vec<_>>();

    results.sort_by(|left, right| {
        right
            .final_score
            .partial_cmp(&left.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                right
                    .memory
                    .observed_at
                    .cmp(&left.memory.observed_at)
                    .then_with(|| left.memory.id.cmp(&right.memory.id))
            })
    });

    for (index, candidate) in results.iter_mut().enumerate() {
        candidate.candidate_rank = index + 1;
    }

    results.truncate(limit);
    results
}

pub fn gate_candidates(candidates: &[ScoredMemory], max_injected: usize) -> GateResult {
    let mut selected = Vec::new();
    let mut seen_lineages = HashSet::new();
    for candidate in candidates {
        if candidate.final_score < GATE_THRESHOLD {
            continue;
        }
        if !seen_lineages.insert(candidate.memory.lineage_id) {
            continue;
        }
        selected.push(candidate.clone());
        if selected.len() >= max_injected {
            break;
        }
    }

    if selected.is_empty() {
        let top_score = candidates
            .first()
            .map(|candidate| candidate.final_score)
            .unwrap_or(0.0);
        let decision = if top_score >= 0.12 {
            GateDecision::DeferToTool
        } else {
            GateDecision::NoInject
        };
        return GateResult {
            decision,
            confidence: top_score.clamp(0.0, 1.0),
            reason: "weak_relevance".to_string(),
            selected,
        };
    }

    let margin = candidates
        .get(1)
        .map(|next| selected[0].final_score - next.final_score)
        .unwrap_or(selected[0].final_score);
    if margin < 0.025 && selected.len() == 1 {
        return GateResult {
            decision: GateDecision::DeferToTool,
            confidence: selected[0].final_score.clamp(0.0, 1.0),
            reason: "ambiguous_top_match".to_string(),
            selected: Vec::new(),
        };
    }

    let reason = match selected[0].memory.subtype {
        crate::model::MemorySubtype::Preference => "preference_match",
        crate::model::MemorySubtype::Project => "project_match",
        crate::model::MemorySubtype::Goal => "goal_match",
        _ if selected[0].memory.kind == MemoryKind::Episodic => "episodic_match",
        _ => "high_signal_match",
    };

    GateResult {
        decision: GateDecision::InjectCompact,
        confidence: selected[0].final_score.clamp(0.0, 1.0),
        reason: reason.to_string(),
        selected,
    }
}

pub fn build_context_bundle(
    selected: &[ScoredMemory],
    entries: &BTreeMap<uuid::Uuid, crate::model::Entry>,
    artifacts: &BTreeMap<uuid::Uuid, crate::model::Artifact>,
) -> Option<String> {
    if selected.is_empty() {
        return None;
    }

    let mut bundle = String::from("Relevant personal context:\n\n");
    for memory in selected {
        bundle.push_str("- ");
        bundle.push_str(memory.memory.display_text.trim());
        bundle.push('\n');
    }

    bundle.push_str("\nEvidence:\n");
    let mut added = HashSet::new();
    for memory in selected {
        for artifact_id in &memory.memory.source_artifact_ids {
            let Some(artifact) = artifacts.get(artifact_id) else {
                continue;
            };
            let Some(entry) = entries.get(&artifact.entry_id) else {
                continue;
            };
            let key = (entry.id, artifact.id);
            if !added.insert(key) {
                continue;
            }
            let snippet_source = entry
                .raw_text
                .as_deref()
                .unwrap_or(&artifact.display_text)
                .chars()
                .take(120)
                .collect::<String>();
            bundle.push_str("- ");
            bundle.push_str(&entry.captured_at.format("%b %d, %Y").to_string());
            bundle.push_str(": \"");
            bundle.push_str(snippet_source.trim());
            bundle.push_str("\"\n");
        }
    }

    Some(bundle.trim_end().to_string())
}

pub fn build_trace(
    query: String,
    recent_context: Option<String>,
    active_thread_id: Option<uuid::Uuid>,
    gate: &GateResult,
    context: Option<String>,
    candidates: &[ScoredMemory],
    created_at: DateTime<Utc>,
) -> RetrievalTrace {
    let id = uuid::Uuid::new_v4();
    let selected_ids = gate
        .selected
        .iter()
        .map(|candidate| candidate.memory.id)
        .collect::<Vec<_>>();

    let candidate_rows = candidates
        .iter()
        .map(|candidate| {
            let injected_rank = gate
                .selected
                .iter()
                .position(|selected| selected.memory.id == candidate.memory.id)
                .map(|rank| rank + 1);
            RetrievalTraceCandidate {
                memory_id: candidate.memory.id,
                lineage_id: candidate.memory.lineage_id,
                semantic_score: candidate.semantic_score,
                lexical_score: candidate.lexical_score,
                fusion_score: candidate.fusion_score,
                temporal_bonus: candidate.temporal_bonus,
                thread_bonus: candidate.thread_bonus,
                salience_bonus: candidate.salience_bonus,
                confidence_bonus: candidate.confidence_bonus,
                reinjection_penalty: candidate.reinjection_penalty,
                stale_penalty: candidate.stale_penalty,
                final_score: candidate.final_score,
                candidate_rank: candidate.candidate_rank,
                selected: injected_rank.is_some(),
                injected_rank,
            }
        })
        .collect::<Vec<_>>();

    RetrievalTrace {
        id,
        query,
        recent_context,
        active_thread_id,
        gate_decision: gate.decision,
        gate_confidence: gate.confidence,
        gate_reason: gate.reason.clone(),
        final_context: context,
        candidates: candidate_rows,
        selected_memory_ids: selected_ids,
        created_at,
    }
}

pub fn rebuild_profile_blocks(
    memories: &BTreeMap<uuid::Uuid, MemoryRecord>,
    threads: &BTreeMap<uuid::Uuid, Thread>,
    now: DateTime<Utc>,
) -> BTreeMap<ProfileLabel, ProfileBlock> {
    let accepted = memories
        .values()
        .filter(|memory| memory.state == MemoryState::Accepted)
        .collect::<Vec<_>>();

    let identity_lines = accepted
        .iter()
        .filter(|memory| memory.subtype == crate::model::MemorySubtype::Person)
        .map(|memory| memory.display_text.clone())
        .take(5)
        .collect::<Vec<_>>();

    let preference_lines = accepted
        .iter()
        .filter(|memory| memory.subtype == crate::model::MemorySubtype::Preference)
        .map(|memory| memory.display_text.clone())
        .take(5)
        .collect::<Vec<_>>();

    let active_threads = threads
        .values()
        .filter(|thread| thread.status == crate::model::ThreadStatus::Active)
        .map(|thread| thread.title.clone())
        .collect::<Vec<_>>();

    let mut blocks = BTreeMap::new();
    blocks.insert(
        ProfileLabel::Identity,
        ProfileBlock {
            label: ProfileLabel::Identity,
            text: join_or_default(identity_lines, "No stored identity context yet."),
            updated_at: now,
        },
    );
    blocks.insert(
        ProfileLabel::Preferences,
        ProfileBlock {
            label: ProfileLabel::Preferences,
            text: join_or_default(
                preference_lines,
                "No durable preference memories have been accepted yet.",
            ),
            updated_at: now,
        },
    );
    blocks.insert(
        ProfileLabel::ActiveThreads,
        ProfileBlock {
            label: ProfileLabel::ActiveThreads,
            text: if active_threads.is_empty() {
                "No active project or life-theme threads are open yet.".to_string()
            } else {
                active_threads
                    .iter()
                    .map(|title| format!("- {title}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            },
            updated_at: now,
        },
    );
    blocks
}

fn join_or_default(lines: Vec<String>, default: &str) -> String {
    if lines.is_empty() {
        default.to_string()
    } else {
        lines
            .into_iter()
            .map(|line| format!("- {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn lexical_rankings(query: &str, memories: &[MemoryRecord]) -> Vec<(uuid::Uuid, f32)> {
    let query_tokens = tokenize(query);
    let mut scores = memories
        .iter()
        .map(|memory| {
            let memory_tokens = tokenize(&memory.retrieval_text);
            let intersection = query_tokens.intersection(&memory_tokens).count() as f32;
            let denom = ((query_tokens.len().max(1) * memory_tokens.len().max(1)) as f32).sqrt();
            let score = if denom == 0.0 {
                0.0
            } else {
                intersection / denom
            };
            (memory.id, score)
        })
        .filter(|(_, score)| *score > 0.0)
        .collect::<Vec<_>>();
    scores.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scores
}

fn semantic_rankings(
    query: &str,
    query_embedding: Option<&crate::model::EmbeddingVector>,
    memories: &[MemoryRecord],
) -> Vec<(uuid::Uuid, f32)> {
    let fallback_query_embedding = embed(query);
    let mut scores = memories
        .iter()
        .map(|memory| {
            let score = match (query_embedding, memory.embedding.as_ref()) {
                (Some(query_embedding), Some(memory_embedding)) => {
                    cosine_similarity(&query_embedding.values, &memory_embedding.values)
                }
                (None, None) => {
                    cosine_similarity(&fallback_query_embedding, &embed(&memory.retrieval_text))
                }
                _ => 0.0,
            };
            (memory.id, score)
        })
        .collect::<Vec<_>>();
    scores.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scores
}

fn embed(text: &str) -> Vec<f32> {
    let mut vector = vec![0.0f32; EMBEDDING_DIMS];
    for token in tokenize(text) {
        let hash = fnv1a(&token);
        let index = (hash as usize) % EMBEDDING_DIMS;
        let sign = if (hash >> 31) & 1 == 0 { 1.0 } else { -1.0 };
        vector[index] += sign;
    }
    vector
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    let mut dot = 0.0;
    let mut left_norm = 0.0;
    let mut right_norm = 0.0;
    for (l, r) in left.iter().zip(right.iter()) {
        dot += l * r;
        left_norm += l * l;
        right_norm += r * r;
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        0.0
    } else {
        dot / (left_norm.sqrt() * right_norm.sqrt())
    }
}

fn tokenize(text: &str) -> BTreeSet<String> {
    text.split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

fn overlaps(
    left_start: DateTime<Utc>,
    left_end: DateTime<Utc>,
    right_start: DateTime<Utc>,
    right_end: DateTime<Utc>,
) -> bool {
    left_start <= right_end && right_start <= left_end
}

fn temporal_bonus(
    memory: &MemoryRecord,
    request: &AssembleContextRequest,
    now: DateTime<Utc>,
) -> f32 {
    if let Some(focus_from) = request.focus_from {
        let focus_to = request.focus_to.unwrap_or(now);
        let valid_overlap = overlaps(
            memory.valid_from,
            memory.valid_to.unwrap_or(DateTime::<Utc>::MAX_UTC),
            focus_from,
            focus_to,
        );
        if valid_overlap { 0.12 } else { 0.0 }
    } else if memory
        .valid_to
        .map(|valid_to| valid_to > now)
        .unwrap_or(true)
    {
        0.08
    } else {
        0.0
    }
}

fn reinjection_penalty(
    memory_id: uuid::Uuid,
    traces: &BTreeMap<uuid::Uuid, RetrievalTrace>,
    now: DateTime<Utc>,
) -> f32 {
    let last = traces
        .values()
        .filter(|trace| trace.selected_memory_ids.contains(&memory_id))
        .map(|trace| trace.created_at)
        .max();
    match last {
        Some(timestamp) if timestamp >= now - Duration::days(1) => 0.18,
        Some(timestamp) if timestamp >= now - Duration::days(7) => 0.08,
        _ => 0.0,
    }
}

fn stale_penalty(
    memory: &MemoryRecord,
    request: &AssembleContextRequest,
    now: DateTime<Utc>,
) -> f32 {
    if request.focus_from.is_none()
        && matches!(memory.kind, MemoryKind::Semantic | MemoryKind::Procedural)
        && memory
            .valid_to
            .map(|valid_to| valid_to <= now)
            .unwrap_or(false)
    {
        0.25
    } else {
        0.0
    }
}

fn rrf(rank: usize) -> f32 {
    if rank == usize::MAX {
        0.0
    } else {
        1.0 / (RRF_K + rank as f32)
    }
}

fn fnv1a(value: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in value.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use serde_json::json;

    use crate::model::{AssembleContextRequest, MemorySubtype, empty_object, now_utc};

    use super::*;

    fn memory(text: &str, subtype: MemorySubtype) -> MemoryRecord {
        let now = now_utc();
        MemoryRecord {
            id: uuid::Uuid::new_v4(),
            lineage_id: uuid::Uuid::new_v4(),
            kind: MemoryKind::Semantic,
            subtype,
            display_text: text.to_string(),
            retrieval_text: text.to_string(),
            attrs: json!({}),
            observed_at: Some(now),
            valid_from: now,
            valid_to: None,
            confidence: 0.9,
            salience: 0.9,
            state: MemoryState::Accepted,
            embedding: None,
            source_artifact_ids: Vec::new(),
            thread_id: None,
            parent_id: None,
            path: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn gate_prefers_high_signal_unique_lineages() {
        let first = ScoredMemory {
            memory: memory(
                "You prefer Rust for backend services.",
                MemorySubtype::Preference,
            ),
            semantic_score: 0.8,
            lexical_score: 0.8,
            fusion_score: 0.5,
            temporal_bonus: 0.08,
            thread_bonus: 0.0,
            salience_bonus: 0.1,
            confidence_bonus: 0.1,
            reinjection_penalty: 0.0,
            stale_penalty: 0.0,
            final_score: 0.78,
            prior_injected: false,
            candidate_rank: 1,
        };
        let mut duplicate = first.clone();
        duplicate.memory.id = uuid::Uuid::new_v4();
        duplicate.candidate_rank = 2;
        let second = ScoredMemory {
            memory: memory(
                "You are building a personal memory system.",
                MemorySubtype::Project,
            ),
            semantic_score: 0.7,
            lexical_score: 0.6,
            fusion_score: 0.45,
            temporal_bonus: 0.08,
            thread_bonus: 0.0,
            salience_bonus: 0.09,
            confidence_bonus: 0.09,
            reinjection_penalty: 0.0,
            stale_penalty: 0.0,
            final_score: 0.71,
            prior_injected: false,
            candidate_rank: 3,
        };

        let gate = gate_candidates(&[first.clone(), duplicate, second.clone()], 3);
        assert_eq!(gate.decision, GateDecision::InjectCompact);
        assert_eq!(gate.selected.len(), 2);
        assert_eq!(gate.reason, "preference_match");
        assert_eq!(gate.selected[0].memory.id, first.memory.id);
        assert_eq!(gate.selected[1].memory.id, second.memory.id);
    }

    #[test]
    fn scoring_penalizes_stale_current_fact() {
        let now = now_utc();
        let mut memories = BTreeMap::new();
        let stale = MemoryRecord {
            id: uuid::Uuid::new_v4(),
            lineage_id: uuid::Uuid::new_v4(),
            kind: MemoryKind::Semantic,
            subtype: MemorySubtype::Project,
            display_text: "You are using a retired stack.".to_string(),
            retrieval_text: "using a retired stack".to_string(),
            attrs: empty_object(),
            observed_at: Some(now - Duration::days(20)),
            valid_from: now - Duration::days(20),
            valid_to: Some(now - Duration::days(2)),
            confidence: 0.8,
            salience: 0.8,
            state: MemoryState::Accepted,
            embedding: None,
            source_artifact_ids: Vec::new(),
            thread_id: None,
            parent_id: None,
            path: None,
            created_at: now,
            updated_at: now,
        };
        memories.insert(stale.id, stale.clone());

        let scored = score_memories(
            SearchEnvironment {
                memories: &memories,
                threads: &BTreeMap::new(),
                retrieval_traces: &BTreeMap::new(),
            },
            &AssembleContextRequest {
                query: "What stack am I using now?".to_string(),
                recent_turns: Vec::new(),
                recent_context: None,
                active_thread_id: None,
                focus_from: None,
                focus_to: None,
                query_embedding: None,
                max_candidates: None,
                max_injected: None,
            },
            10,
            now,
        );

        assert!(scored.is_empty());
    }
}
