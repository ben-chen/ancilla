-- Personal LLM memory system v1 schema.
-- Notes:
-- 1. This file targets Aurora/PostgreSQL with pgvector.
-- 2. The v1 embedding plan uses the Perplexity 0.6B embedding family:
--    - `perplexity-ai/pplx-embed-v1-0.6b` for query and memory embeddings
--    - `perplexity-ai/pplx-embed-context-v1-0.6b` for artifact/chunk embeddings
-- 3. Both models emit 1024-dimensional embeddings and are compared with cosine
--    similarity. This schema stores widened float vectors in pgvector while
--    preserving the original quantization mode in table metadata.
-- 4. Lexical retrieval uses built-in Postgres full text search for Aurora
--    compatibility. If exact BM25 becomes a hard requirement, move the lexical
--    leg to a dedicated search system or a Postgres distribution that supports it.

CREATE EXTENSION IF NOT EXISTS pgcrypto;
CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS btree_gist;

CREATE OR REPLACE FUNCTION touch_updated_at()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
  NEW.updated_at = now();
  RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION forbid_row_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
  RAISE EXCEPTION 'table % is append-only', TG_TABLE_NAME;
END;
$$;

CREATE TABLE entries (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  kind text NOT NULL CHECK (
    kind IN ('text_journal', 'audio_dictation', 'chat_turn', 'import')
  ),
  raw_text text,
  asset_ref text,
  captured_at timestamptz NOT NULL,
  timezone text NOT NULL,
  source_app text,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  CHECK (raw_text IS NOT NULL OR asset_ref IS NOT NULL)
);

CREATE TRIGGER entries_append_only
BEFORE UPDATE ON entries
FOR EACH ROW
EXECUTE FUNCTION forbid_row_update();

CREATE TABLE artifacts (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  entry_id uuid NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
  kind text NOT NULL CHECK (
    kind IN ('transcript', 'chunk', 'summary', 'reflection')
  ),
  ordinal integer NOT NULL DEFAULT 0,
  display_text text NOT NULL,
  retrieval_text text NOT NULL,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  search_vector tsvector GENERATED ALWAYS AS (
    setweight(to_tsvector('english', coalesce(display_text, '')), 'A') ||
    setweight(to_tsvector('english', coalesce(retrieval_text, '')), 'B') ||
    setweight(
      jsonb_to_tsvector('english', metadata, '["string"]'::jsonb),
      'C'
    )
  ) STORED
);

CREATE INDEX artifacts_entry_id_idx ON artifacts (entry_id, ordinal);
CREATE INDEX artifacts_search_vector_idx ON artifacts USING gin (search_vector);

CREATE TABLE artifact_embeddings (
  artifact_id uuid NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
  embedding_model text NOT NULL,
  embedding_version text NOT NULL,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  quantization text NOT NULL DEFAULT 'int8' CHECK (
    quantization IN ('float32', 'int8', 'binary')
  ),
  normalized boolean NOT NULL DEFAULT false,
  dims integer NOT NULL DEFAULT 1024 CHECK (dims = 1024),
  embedding vector(1024) NOT NULL,
  active boolean NOT NULL DEFAULT true,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (artifact_id, embedding_model, embedding_version)
);

CREATE UNIQUE INDEX artifact_embeddings_one_active_idx
  ON artifact_embeddings (artifact_id)
  WHERE active;

CREATE INDEX artifact_embeddings_active_hnsw_idx
  ON artifact_embeddings
  USING hnsw (embedding vector_cosine_ops)
  WHERE active;

CREATE TABLE threads (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  kind text NOT NULL CHECK (kind IN ('project', 'life_theme')),
  title text NOT NULL,
  summary text NOT NULL DEFAULT '',
  status text NOT NULL CHECK (status IN ('active', 'dormant', 'closed')),
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TRIGGER threads_touch_updated_at
BEFORE UPDATE ON threads
FOR EACH ROW
EXECUTE FUNCTION touch_updated_at();

CREATE TABLE memory_records (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  lineage_id uuid NOT NULL DEFAULT gen_random_uuid(),
  kind text NOT NULL CHECK (kind IN ('semantic', 'episodic', 'procedural')),
  subtype text NOT NULL CHECK (
    subtype IN ('preference', 'project', 'habit', 'person', 'place', 'goal')
  ),
  display_text text NOT NULL,
  retrieval_text text NOT NULL,
  attrs jsonb NOT NULL DEFAULT '{}'::jsonb,
  observed_at timestamptz,
  valid_from timestamptz NOT NULL,
  valid_to timestamptz,
  confidence double precision NOT NULL DEFAULT 0.5 CHECK (
    confidence >= 0.0 AND confidence <= 1.0
  ),
  salience double precision NOT NULL DEFAULT 0.5 CHECK (
    salience >= 0.0 AND salience <= 1.0
  ),
  state text NOT NULL CHECK (
    state IN ('candidate', 'accepted', 'superseded', 'rejected', 'deleted')
  ),
  thread_id uuid REFERENCES threads(id) ON DELETE SET NULL,
  parent_id uuid REFERENCES memory_records(id) ON DELETE SET NULL,
  supersedes_id uuid UNIQUE REFERENCES memory_records(id) ON DELETE SET NULL,
  path text,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  valid_during tstzrange GENERATED ALWAYS AS (
    tstzrange(valid_from, coalesce(valid_to, 'infinity'::timestamptz), '[)')
  ) STORED,
  search_vector tsvector GENERATED ALWAYS AS (
    setweight(to_tsvector('english', coalesce(display_text, '')), 'A') ||
    setweight(to_tsvector('english', coalesce(retrieval_text, '')), 'B') ||
    setweight(
      jsonb_to_tsvector('english', attrs, '["string"]'::jsonb),
      'C'
    )
  ) STORED,
  CHECK (valid_to IS NULL OR valid_to > valid_from)
);

CREATE TRIGGER memory_records_touch_updated_at
BEFORE UPDATE ON memory_records
FOR EACH ROW
EXECUTE FUNCTION touch_updated_at();

ALTER TABLE memory_records
  ADD CONSTRAINT memory_records_no_overlapping_acceptance
  EXCLUDE USING gist (
    lineage_id WITH =,
    valid_during WITH &&
  )
  WHERE (state = 'accepted');

CREATE INDEX memory_records_state_kind_idx
  ON memory_records (state, kind, subtype);

CREATE INDEX memory_records_lineage_idx
  ON memory_records (lineage_id);

CREATE INDEX memory_records_thread_idx
  ON memory_records (thread_id)
  WHERE thread_id IS NOT NULL;

CREATE INDEX memory_records_observed_at_idx
  ON memory_records (observed_at DESC);

CREATE INDEX memory_records_valid_during_idx
  ON memory_records USING gist (valid_during);

CREATE INDEX memory_records_search_vector_idx
  ON memory_records USING gin (search_vector);

CREATE TABLE memory_sources (
  memory_id uuid NOT NULL REFERENCES memory_records(id) ON DELETE CASCADE,
  artifact_id uuid NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
  evidence_rank smallint NOT NULL DEFAULT 0,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (memory_id, artifact_id)
);

CREATE INDEX memory_sources_artifact_id_idx
  ON memory_sources (artifact_id);

CREATE TABLE memory_embeddings (
  memory_id uuid NOT NULL REFERENCES memory_records(id) ON DELETE CASCADE,
  embedding_model text NOT NULL,
  embedding_version text NOT NULL,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  quantization text NOT NULL DEFAULT 'int8' CHECK (
    quantization IN ('float32', 'int8', 'binary')
  ),
  normalized boolean NOT NULL DEFAULT false,
  dims integer NOT NULL DEFAULT 1024 CHECK (dims = 1024),
  embedding vector(1024) NOT NULL,
  active boolean NOT NULL DEFAULT true,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (memory_id, embedding_model, embedding_version)
);

CREATE UNIQUE INDEX memory_embeddings_one_active_idx
  ON memory_embeddings (memory_id)
  WHERE active;

CREATE INDEX memory_embeddings_active_hnsw_idx
  ON memory_embeddings
  USING hnsw (embedding vector_cosine_ops)
  WHERE active;

CREATE TABLE profile_blocks (
  label text PRIMARY KEY CHECK (
    label IN ('identity', 'preferences', 'active_threads')
  ),
  text text NOT NULL,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TRIGGER profile_blocks_touch_updated_at
BEFORE UPDATE ON profile_blocks
FOR EACH ROW
EXECUTE FUNCTION touch_updated_at();

CREATE TABLE profile_block_sources (
  profile_label text NOT NULL REFERENCES profile_blocks(label) ON DELETE CASCADE,
  memory_id uuid NOT NULL REFERENCES memory_records(id) ON DELETE CASCADE,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (profile_label, memory_id)
);

CREATE TABLE retrieval_traces (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  query_text text NOT NULL,
  recent_context text,
  active_thread_id uuid REFERENCES threads(id) ON DELETE SET NULL,
  query_embedding_model text NOT NULL,
  gate_decision text CHECK (
    gate_decision IN ('no_inject', 'inject_compact', 'defer_to_tool')
  ),
  gate_confidence double precision CHECK (
    gate_confidence IS NULL OR (gate_confidence >= 0.0 AND gate_confidence <= 1.0)
  ),
  gate_reason text,
  final_context text,
  request_metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX retrieval_traces_created_at_idx
  ON retrieval_traces (created_at DESC);

CREATE TABLE retrieval_trace_candidates (
  trace_id uuid NOT NULL REFERENCES retrieval_traces(id) ON DELETE CASCADE,
  memory_id uuid NOT NULL REFERENCES memory_records(id) ON DELETE CASCADE,
  candidate_rank integer NOT NULL,
  semantic_rank integer,
  lexical_rank integer,
  semantic_score double precision,
  lexical_score double precision,
  fusion_score double precision NOT NULL,
  temporal_bonus double precision NOT NULL DEFAULT 0.0,
  thread_bonus double precision NOT NULL DEFAULT 0.0,
  salience_bonus double precision NOT NULL DEFAULT 0.0,
  confidence_bonus double precision NOT NULL DEFAULT 0.0,
  reinjection_penalty double precision NOT NULL DEFAULT 0.0,
  stale_penalty double precision NOT NULL DEFAULT 0.0,
  final_score double precision NOT NULL,
  gate_label text,
  gate_score double precision,
  selected boolean NOT NULL DEFAULT false,
  injected_rank integer,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (trace_id, memory_id)
);

CREATE INDEX retrieval_trace_candidates_memory_idx
  ON retrieval_trace_candidates (memory_id, created_at DESC);

CREATE VIEW memory_evidence AS
SELECT
  m.id AS memory_id,
  m.display_text AS memory_text,
  a.id AS artifact_id,
  a.kind AS artifact_kind,
  a.display_text AS artifact_text,
  e.id AS entry_id,
  e.kind AS entry_kind,
  e.captured_at,
  e.raw_text AS entry_text,
  ms.evidence_rank
FROM memory_records m
JOIN memory_sources ms ON ms.memory_id = m.id
JOIN artifacts a ON a.id = ms.artifact_id
JOIN entries e ON e.id = a.entry_id;
