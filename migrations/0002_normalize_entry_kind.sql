ALTER TABLE entries DISABLE TRIGGER entries_append_only;

ALTER TABLE entries DROP CONSTRAINT IF EXISTS entries_kind_check;

UPDATE entries
SET metadata = CASE
  WHEN metadata ? 'source_modality' THEN metadata
  WHEN kind = 'audio_dictation' THEN jsonb_set(metadata, '{source_modality}', to_jsonb('audio'::text), true)
  ELSE jsonb_set(metadata, '{source_modality}', to_jsonb('text'::text), true)
END
WHERE kind IN ('text_journal', 'audio_dictation');

UPDATE entries
SET kind = 'text'
WHERE kind IN ('text_journal', 'audio_dictation');

ALTER TABLE entries
  ADD CONSTRAINT entries_kind_check CHECK (kind IN ('text', 'chat_turn', 'import'));

ALTER TABLE entries ENABLE TRIGGER entries_append_only;
