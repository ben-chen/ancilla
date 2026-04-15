ALTER TABLE memory_records
  ADD COLUMN title text,
  ADD COLUMN tags text[] NOT NULL DEFAULT '{}'::text[],
  ADD COLUMN content_markdown text,
  ADD COLUMN search_text text;

UPDATE memory_records
SET
  title = COALESCE(NULLIF(trim(display_text), ''), 'Untitled Memory'),
  tags = CASE subtype
    WHEN 'preference' THEN ARRAY['preference']
    WHEN 'project' THEN ARRAY['project']
    WHEN 'habit' THEN ARRAY['habit']
    WHEN 'person' THEN ARRAY['person']
    WHEN 'place' THEN ARRAY['place']
    WHEN 'goal' THEN ARRAY['goal']
    ELSE ARRAY[]::text[]
  END,
  content_markdown = concat(
    '# ',
    replace(COALESCE(NULLIF(trim(display_text), ''), 'Untitled Memory'), E'\n', ' '),
    E'\n', E'\n',
    CASE
      WHEN subtype IN ('preference', 'project', 'habit', 'person', 'place', 'goal')
        THEN concat('Tags: ', subtype, E'\n', E'\n')
      ELSE ''
    END,
    COALESCE(
      NULLIF(trim(retrieval_text), ''),
      NULLIF(trim(display_text), ''),
      'Untitled Memory'
    )
  ),
  search_text = trim(concat_ws(
    E'\n',
    COALESCE(NULLIF(trim(display_text), ''), 'Untitled Memory'),
    array_to_string(
      CASE subtype
        WHEN 'preference' THEN ARRAY['preference']
        WHEN 'project' THEN ARRAY['project']
        WHEN 'habit' THEN ARRAY['habit']
        WHEN 'person' THEN ARRAY['person']
        WHEN 'place' THEN ARRAY['place']
        WHEN 'goal' THEN ARRAY['goal']
        ELSE ARRAY[]::text[]
      END,
      ' '
    ),
    COALESCE(NULLIF(trim(retrieval_text), ''), trim(display_text))
  ));

ALTER TABLE memory_records
  ALTER COLUMN title SET NOT NULL,
  ALTER COLUMN content_markdown SET NOT NULL,
  ALTER COLUMN search_text SET NOT NULL;

DROP INDEX IF EXISTS memory_records_state_kind_idx;
DROP INDEX IF EXISTS memory_records_search_vector_idx;
DROP VIEW IF EXISTS memory_evidence;

ALTER TABLE memory_records
  DROP COLUMN search_vector;

ALTER TABLE memory_records
  DROP COLUMN subtype,
  DROP COLUMN display_text,
  DROP COLUMN retrieval_text,
  DROP COLUMN confidence,
  DROP COLUMN salience;

ALTER TABLE memory_records
  ADD COLUMN search_vector tsvector GENERATED ALWAYS AS (
    setweight(to_tsvector('english', coalesce(title, '')), 'A') ||
    setweight(to_tsvector('english', coalesce(search_text, '')), 'B') ||
    setweight(
      jsonb_to_tsvector('english', attrs, '["string"]'::jsonb),
      'C'
    )
  ) STORED;

CREATE INDEX memory_records_state_kind_idx
  ON memory_records (state, kind);

CREATE INDEX memory_records_search_vector_idx
  ON memory_records USING gin (search_vector);

CREATE INDEX memory_records_tags_idx
  ON memory_records USING gin (tags);

CREATE VIEW memory_evidence AS
SELECT
  m.id AS memory_id,
  m.content_markdown AS memory_text,
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
