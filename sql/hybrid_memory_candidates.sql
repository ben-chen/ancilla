-- Hybrid retrieval query for prompt-time candidate generation.
--
-- Parameters:
--   $1  text            query_text
--   $2  vector(1024)    query_embedding (nullable)
--   $3  timestamptz     turn_at
--   $4  timestamptz     focus_from (nullable)
--   $5  timestamptz     focus_to (nullable)
--   $6  uuid            active_thread_id (nullable)
--   $7  integer         semantic_k
--   $8  integer         lexical_k
--   $9  integer         final_k
--   $10 interval        recent_injection_window
--
-- Output:
--   Candidate memories with hybrid ranks plus policy features that the gate model
--   and deterministic selector need. The intended flow is:
--   1. Run this query every turn.
--   2. Send the top 10-15 rows to the gate model.
--   3. Apply deterministic token and margin rules before final injection.
--
-- Notes:
--   * This uses reciprocal rank fusion for stable hybrid retrieval.
--   * Query embeddings for this path should come from
--     `perplexity-ai/pplx-embed-v1-0.6b`, matching the memory embedding index.
--   * If `query_embedding` is null, the semantic leg drops out and the query runs
--     lexical retrieval plus policy-aware reranking only.
--   * Contextual artifact embeddings from `pplx-embed-context-v1-0.6b` belong in
--     a separate retrieval path for chunk/evidence lookups.
--   * The lexical leg uses `ts_rank_cd`, which is Aurora-friendly but not exact BM25.
--   * `focus_from` / `focus_to` should be filled when the user asks a historical
--     question such as "what was I doing in February?".

WITH input AS (
  SELECT
    $1::text AS query_text,
    $2::vector(1024) AS query_embedding,
    $3::timestamptz AS turn_at,
    $4::timestamptz AS focus_from,
    $5::timestamptz AS focus_to,
    $6::uuid AS active_thread_id,
    GREATEST($7::integer, 0) AS semantic_k,
    GREATEST($8::integer, 0) AS lexical_k,
    GREATEST($9::integer, 1) AS final_k,
    COALESCE($10::interval, interval '7 days') AS recent_injection_window
),
context AS (
  SELECT
    query_text,
    query_embedding,
    turn_at,
    active_thread_id,
    semantic_k,
    lexical_k,
    final_k,
    recent_injection_window,
    (focus_from IS NOT NULL OR focus_to IS NOT NULL) AS has_focus_window,
    tstzrange(
      coalesce(focus_from, '-infinity'::timestamptz),
      coalesce(focus_to, 'infinity'::timestamptz),
      '[)'
    ) AS focus_window,
    CASE
      WHEN btrim(query_text) = '' THEN NULL
      ELSE websearch_to_tsquery('english', query_text)
    END AS ts_query
  FROM input
),
eligible AS (
  SELECT
    m.id,
    m.lineage_id,
    m.kind,
    m.subtype,
    m.display_text,
    m.retrieval_text,
    m.attrs,
    m.thread_id,
    m.observed_at,
    m.valid_from,
    m.valid_to,
    m.valid_during,
    m.confidence,
    m.salience,
    m.search_vector
  FROM memory_records m
  CROSS JOIN context c
  WHERE m.state = 'accepted'
    AND (
      CASE
        WHEN c.has_focus_window THEN
          m.valid_during && c.focus_window
          OR (
            m.observed_at IS NOT NULL
            AND tstzrange(
              m.observed_at,
              m.observed_at + interval '1 second',
              '[]'
            ) && c.focus_window
          )
        ELSE
          m.kind = 'episodic'
          OR m.valid_during @> c.turn_at
      END
    )
),
semantic AS (
  SELECT
    me.memory_id AS id,
    row_number() OVER (
      ORDER BY me.embedding <=> c.query_embedding, e.salience DESC, e.confidence DESC
    ) AS semantic_rank,
    1 - (me.embedding <=> c.query_embedding) AS semantic_score
  FROM memory_embeddings me
  JOIN eligible e ON e.id = me.memory_id
  CROSS JOIN context c
  WHERE me.active = true
    AND c.query_embedding IS NOT NULL
  ORDER BY me.embedding <=> c.query_embedding, e.salience DESC, e.confidence DESC
  LIMIT (SELECT semantic_k FROM context)
),
lexical AS (
  SELECT
    e.id,
    row_number() OVER (
      ORDER BY
        ts_rank_cd(e.search_vector, c.ts_query, 32) DESC,
        e.salience DESC,
        e.confidence DESC
    ) AS lexical_rank,
    ts_rank_cd(e.search_vector, c.ts_query, 32) AS lexical_score
  FROM eligible e
  CROSS JOIN context c
  WHERE c.ts_query IS NOT NULL
    AND e.search_vector @@ c.ts_query
  ORDER BY
    ts_rank_cd(e.search_vector, c.ts_query, 32) DESC,
    e.salience DESC,
    e.confidence DESC
  LIMIT (SELECT lexical_k FROM context)
),
fused AS (
  SELECT
    coalesce(s.id, l.id) AS id,
    s.semantic_rank,
    l.lexical_rank,
    s.semantic_score,
    l.lexical_score,
    coalesce(1.0 / (60 + s.semantic_rank), 0.0) +
    coalesce(1.0 / (60 + l.lexical_rank), 0.0) AS fusion_score
  FROM semantic s
  FULL OUTER JOIN lexical l USING (id)
),
recent_injections AS (
  SELECT
    rtc.memory_id,
    max(rt.created_at) AS last_injected_at,
    count(*) AS injection_count
  FROM retrieval_trace_candidates rtc
  JOIN retrieval_traces rt ON rt.id = rtc.trace_id
  CROSS JOIN context c
  WHERE rtc.selected = true
    AND rt.created_at >= c.turn_at - c.recent_injection_window
  GROUP BY rtc.memory_id
),
scored AS (
  SELECT
    e.id,
    e.lineage_id,
    e.kind,
    e.subtype,
    e.display_text,
    e.retrieval_text,
    e.attrs,
    e.thread_id,
    e.observed_at,
    e.valid_from,
    e.valid_to,
    e.confidence,
    e.salience,
    f.semantic_rank,
    f.lexical_rank,
    f.semantic_score,
    f.lexical_score,
    f.fusion_score,
    CASE
      WHEN e.thread_id IS NOT DISTINCT FROM c.active_thread_id THEN 0.10
      ELSE 0.0
    END AS thread_bonus,
    CASE
      WHEN c.has_focus_window AND e.valid_during && c.focus_window THEN 0.12
      WHEN NOT c.has_focus_window AND e.valid_during @> c.turn_at THEN 0.08
      ELSE 0.0
    END AS temporal_bonus,
    LEAST(e.salience * 0.12, 0.12) AS salience_bonus,
    LEAST(e.confidence * 0.10, 0.10) AS confidence_bonus,
    CASE
      WHEN ri.last_injected_at IS NULL THEN 0.0
      WHEN ri.last_injected_at >= c.turn_at - interval '1 day' THEN 0.18
      ELSE 0.08
    END AS reinjection_penalty,
    CASE
      WHEN NOT c.has_focus_window
        AND e.kind IN ('semantic', 'procedural')
        AND NOT (e.valid_during @> c.turn_at) THEN 0.25
      ELSE 0.0
    END AS stale_penalty,
    (ri.injection_count IS NOT NULL) AS prior_injected
  FROM fused f
  JOIN eligible e ON e.id = f.id
  CROSS JOIN context c
  LEFT JOIN recent_injections ri ON ri.memory_id = e.id
),
finalized AS (
  SELECT
    s.*,
    (
      s.fusion_score +
      s.thread_bonus +
      s.temporal_bonus +
      s.salience_bonus +
      s.confidence_bonus -
      s.reinjection_penalty -
      s.stale_penalty
    ) AS final_score
  FROM scored s
),
ranked AS (
  SELECT
    f.*,
    row_number() OVER (
      ORDER BY
        f.final_score DESC,
        f.observed_at DESC NULLS LAST,
        f.id
    ) AS candidate_rank
  FROM finalized f
)
SELECT
  r.id,
  r.lineage_id,
  r.kind,
  r.subtype,
  r.display_text,
  r.retrieval_text,
  r.attrs,
  r.thread_id,
  r.observed_at,
  r.valid_from,
  r.valid_to,
  r.confidence,
  r.salience,
  r.semantic_rank,
  r.lexical_rank,
  r.semantic_score,
  r.lexical_score,
  r.fusion_score,
  r.temporal_bonus,
  r.thread_bonus,
  r.salience_bonus,
  r.confidence_bonus,
  r.reinjection_penalty,
  r.stale_penalty,
  r.final_score,
  r.prior_injected,
  coalesce(src.source_artifact_ids, ARRAY[]::uuid[]) AS source_artifact_ids,
  r.candidate_rank
FROM ranked r
LEFT JOIN LATERAL (
  SELECT array_agg(ms.artifact_id ORDER BY ms.evidence_rank) AS source_artifact_ids
  FROM memory_sources ms
  WHERE ms.memory_id = r.id
) src ON true
WHERE r.candidate_rank <= (SELECT final_k FROM context)
ORDER BY r.candidate_rank;
