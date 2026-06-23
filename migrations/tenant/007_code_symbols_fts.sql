-- Wave 10 S2: add FTS column + GIN index to code_symbols for hybrid retrieval.
--
-- GENERATED ALWAYS AS ... STORED means Postgres self-maintains the tsvector on
-- insert/update — no application change required in projector-pg or embed-worker.
-- 'simple' config avoids language stemming that hurts code identifiers.
-- The sparse leg of hybrid_search uses ts_rank_cd + plainto_tsquery('simple', $q).
--
-- This migration is additive; the dense path is untouched.
ALTER TABLE code_symbols
  ADD COLUMN IF NOT EXISTS fts tsvector
  GENERATED ALWAYS AS (
    to_tsvector('simple',
      coalesce(fqn, '') || ' ' || coalesce(source_text, ''))
  ) STORED;

CREATE INDEX IF NOT EXISTS idx_code_symbols_fts ON code_symbols USING GIN (fts);
