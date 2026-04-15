WITH context AS (
  SELECT
    $1::text AS query_text,
    $2::vector(1024) AS query_embedding,
    $3::timestamptz AS turn_at,
    $4::timestamptz AS focus_from,
    $5::timestamptz AS focus_to,
    COALESCE($6::uuid[], ARRAY[]::uuid[]) AS banned_ids,
    GREATEST($7::integer, 1) AS final_k,
    CASE
      WHEN btrim($1::text) = '' THEN NULL
      ELSE websearch_to_tsquery('english', $1::text)
    END AS ts_query
),
eligible AS (
  SELECT
    m.id,
    m.lineage_id,
    m.kind,
    m.title,
    m.tags,
    m.content_markdown,
    m.search_text,
    m.attrs,
    m.observed_at,
    m.valid_from,
    m.valid_to,
    m.state,
    m.thread_id,
    m.parent_id,
    m.path,
    m.created_at,
    m.updated_at,
    COALESCE(ms.source_artifact_ids, ARRAY[]::uuid[]) AS source_artifact_ids,
    CASE
      WHEN c.query_embedding IS NOT NULL AND me.embedding IS NOT NULL
        THEN (1 - (me.embedding <=> c.query_embedding))::double precision
      ELSE NULL
    END AS semantic_score,
    CASE
      WHEN c.ts_query IS NOT NULL AND m.search_vector @@ c.ts_query
        THEN ts_rank_cd(m.search_vector, c.ts_query, 32)::double precision
      ELSE 0.0
    END AS lexical_score
  FROM memory_records m
  CROSS JOIN context c
  LEFT JOIN LATERAL (
    SELECT array_agg(ms.artifact_id ORDER BY ms.evidence_rank) AS source_artifact_ids
    FROM memory_sources ms
    WHERE ms.memory_id = m.id
  ) ms ON TRUE
  LEFT JOIN memory_embeddings me
    ON me.memory_id = m.id
   AND me.active = TRUE
  WHERE m.state = 'accepted'
    AND NOT (m.id = ANY(c.banned_ids))
    AND (
      CASE
        WHEN c.focus_from IS NOT NULL OR c.focus_to IS NOT NULL THEN
          tstzrange(
            m.valid_from,
            COALESCE(m.valid_to, 'infinity'::timestamptz),
            '[)'
          ) && tstzrange(
            COALESCE(c.focus_from, '-infinity'::timestamptz),
            COALESCE(c.focus_to, 'infinity'::timestamptz),
            '[)'
          )
          OR (
            m.observed_at IS NOT NULL
            AND m.observed_at >= COALESCE(c.focus_from, '-infinity'::timestamptz)
            AND m.observed_at <= COALESCE(c.focus_to, 'infinity'::timestamptz)
          )
        ELSE
          m.kind = 'episodic'
          OR COALESCE(m.valid_to, 'infinity'::timestamptz) > c.turn_at
      END
    )
),
semantic_all AS (
  SELECT
    e.id,
    e.semantic_score,
    row_number() OVER (
      ORDER BY e.semantic_score DESC NULLS LAST, e.observed_at DESC NULLS LAST, e.id
    ) AS semantic_rank
  FROM eligible e
  WHERE e.semantic_score IS NOT NULL
),
semantic_top AS (
  SELECT id, semantic_rank
  FROM semantic_all
  ORDER BY semantic_rank
  LIMIT 5
),
lexical_base AS (
  SELECT
    e.id,
    e.lexical_score
  FROM eligible e
  WHERE e.lexical_score > 0.0
  ORDER BY e.lexical_score DESC, e.observed_at DESC NULLS LAST, e.id
  LIMIT 50
),
lexical_top AS (
  SELECT
    lb.id,
    row_number() OVER (
      ORDER BY
        COALESCE(e.semantic_score, 0.0) DESC,
        lb.lexical_score DESC,
        e.observed_at DESC NULLS LAST,
        e.id
    ) AS lexical_rank
  FROM lexical_base lb
  JOIN eligible e USING (id)
  LEFT JOIN semantic_top st USING (id)
  WHERE st.id IS NULL
  LIMIT 5
),
candidate_ids AS (
  SELECT id FROM semantic_top
  UNION
  SELECT id FROM lexical_top
),
finalized AS (
  SELECT
    e.*,
    st.semantic_rank,
    lt.lexical_rank,
    CASE
      WHEN COALESCE(e.semantic_score, 0.0) > 0.0 THEN COALESCE(e.semantic_score, 0.0)
      ELSE e.lexical_score
    END AS final_score
  FROM eligible e
  JOIN candidate_ids c USING (id)
  LEFT JOIN semantic_top st USING (id)
  LEFT JOIN lexical_top lt USING (id)
),
ranked AS (
  SELECT
    f.*,
    row_number() OVER (
      ORDER BY f.final_score DESC, f.observed_at DESC NULLS LAST, f.id
    ) AS candidate_rank
  FROM finalized f
)
SELECT
  r.id,
  r.lineage_id,
  r.kind,
  r.title,
  r.tags,
  r.content_markdown,
  r.search_text,
  r.attrs,
  r.observed_at,
  r.valid_from,
  r.valid_to,
  r.state,
  r.thread_id,
  r.parent_id,
  r.path,
  r.created_at,
  r.updated_at,
  r.source_artifact_ids,
  r.semantic_rank,
  r.lexical_rank,
  COALESCE(r.semantic_score, 0.0) AS semantic_score,
  r.lexical_score,
  r.final_score AS fusion_score,
  0.0::double precision AS temporal_bonus,
  0.0::double precision AS thread_bonus,
  0.0::double precision AS salience_bonus,
  0.0::double precision AS confidence_bonus,
  0.0::double precision AS reinjection_penalty,
  0.0::double precision AS stale_penalty,
  r.final_score,
  false AS prior_injected,
  r.candidate_rank
FROM ranked r
ORDER BY r.candidate_rank
LIMIT (SELECT final_k FROM context);
