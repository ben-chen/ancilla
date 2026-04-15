# SQL Schema And Retrieval Design

This turns the spec into a storage and retrieval shape that is practical on Aurora/PostgreSQL, preserves temporal truth, and keeps prompt injection constrained.

## Embedding Choice

The v1 spec now assumes the Perplexity `pplx-embed-v1-0.6b` model rather than Titan for query embeddings, compact `memory_records`, and any artifact embeddings we persist today.

That changes a few important storage assumptions:

- embedding dimensionality is `1024`
- max context length is `32K`
- no instruction prefix is required for indexing or querying
- similarity should be cosine, not dot product
- the native outputs are unnormalized `int8` or binary embeddings

For v1 on pgvector, the schema stores widened float vectors in `vector(1024)` and preserves the original quantization mode in metadata. If binary retrieval materially improves recall/latency later, add a sidecar `bit` representation and benchmark it separately instead of mixing storage formats in the main index.

## Core Decisions

`entries` is the only immutable source-of-truth table.
Raw text or asset references land there and never change.

`artifacts` is the derived-content plane.
Transcripts, chunks, summaries, and reflections live here so extraction can be replayed without mutating source data.

`artifact_embeddings` is separate from `memory_embeddings`.
That keeps chunk-level evidence embeddings in their own lane instead of mixing them into the recall index for compact memories, even though both currently use the same base model.

`memory_records` is the recall plane.
Only compact, explainable memory units are retrieved into conversation context.

`memory_sources` replaces `source_artifact_ids: []`.
Arrays are fine for transport; they are weak for joins, evidence reads, and traceability.

`lineage_id` plus `supersedes_id` handles temporal updates.
When a fact changes, insert a new memory version in the same lineage and supersede the old row instead of editing the old text in place.

`valid_during` is a stored `tstzrange`.
This is the simplest way to support both “what is true now?” and “what was true in February?” without branching the schema.

Accepted versions in the same lineage cannot overlap.
That is enforced in SQL with an exclusion constraint, not left to application discipline.

`retrieval_traces` and `retrieval_trace_candidates` make injection auditable.
Every candidate can be reconstructed with the hybrid scores and policy penalties that produced it.

## Why The Lexical Leg Uses FTS Instead Of BM25

Aurora/PostgreSQL gives you `tsvector` and `ts_rank_cd` without extra infrastructure.
That is not exact BM25, but it is operationally simple and good enough for v1 hybrid retrieval.

If exact BM25 meaningfully improves quality later, move only the lexical leg, not the whole memory system:

- PostgreSQL distribution with BM25 support
- OpenSearch
- Dedicated reranker after candidate generation

The schema here does not block that swap.

## Retrieval Flow

Per turn:

1. Build a query from the live user message, the recent turns, and the active thread summary.
2. Resolve optional temporal focus into `focus_from` / `focus_to`.
3. Run [`sql/hybrid_memory_candidates.sql`](/Users/benchen/workspace/ancilla/sql/hybrid_memory_candidates.sql).
4. Send the top 10-15 candidates to the small gate model.
5. Apply deterministic selection rules before prompt injection.
6. Persist the trace and selected memories.

Artifact-level retrieval is a separate path.
Use the same embed model for evidence reads, timeline drill-down, and future `personal_context.read_evidence` style tool calls, not for the primary memory injection index.

## Deterministic Rules After The Gate

The SQL query returns candidate features, not a final prompt bundle. The final policy should stay in application code so it can evolve without rewriting SQL.

Recommended v1 policy:

- Never inject more than 3 memories on a turn.
- Cap injected memory to roughly 180-220 tokens total.
- Require `final_score >= 0.18` for any candidate to be gate-eligible.
- If the gate selects multiple items from the same `lineage_id`, keep the highest-scoring current version only.
- If the top selected memory beats the next candidate by less than `0.025`, downgrade to `inject_compact` or `no_inject`.
- If no candidate survives both score threshold and token budget, inject nothing and rely on tool use.

## Query Shape

The candidate query has four stages:

1. Eligibility:
`accepted` memories only, with temporal filtering that changes when the user asks a historical question.

2. Hybrid candidate generation:
semantic ANN over `memory_embeddings` plus lexical search over `memory_records.search_vector`.

3. Fusion:
reciprocal rank fusion so one noisy scoring scale cannot dominate the other.

4. Policy-aware rerank:
boost active thread match, currently valid memories, salience, and confidence; penalize recent reinjection and stale semantic facts.

This keeps retrieval cheap and deterministic while letting the learned gate decide whether anything should actually reach the prompt.

## Practical Limits

Recommended v1 defaults:

- `semantic_k = 40`
- `lexical_k = 40`
- `final_k = 20`
- reinjection window: `7 days`

That gives enough diversity for the gate without dragging too much irrelevant context into the expensive part of the pipeline.

## One Important Constraint

The schema now assumes `pplx-embed-v1-0.6b` at `1024` dimensions. If you swap models later, rebuild embeddings consistently rather than mixing embedding spaces inside the same ANN index.
