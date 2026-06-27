-- Wave 10: safety-net backfill for the fts column introduced in 007_code_symbols_fts.
--
-- Tenant schemas created before 007_code_symbols_fts was added had a prior
-- migration recorded under version 7 ("agent sessions", since deleted).  The
-- idempotency check in migrate-tenant.sh skips any migration whose version is
-- already in schema_migrations, so those schemas never received the fts column.
--
-- This migration re-applies the identical DDL under a fresh version number so
-- the runner picks it up for every schema that was skipped.  Both statements
-- are idempotent (IF NOT EXISTS) so schemas that already have the column and
-- index are unaffected.
ALTER TABLE code_symbols
  ADD COLUMN IF NOT EXISTS fts tsvector
  GENERATED ALWAYS AS (
    to_tsvector('simple',
      coalesce(fqn, '') || ' ' || coalesce(source_text, ''))
  ) STORED;

CREATE INDEX IF NOT EXISTS idx_code_symbols_fts ON code_symbols USING GIN (fts);
