-- Remove broken ivfflat index from tenants where 002_code_tables.sql was applied
-- before ON_ERROR_STOP was enforced (the index creation silently failed then).
-- A replacement vector-similarity index is intentionally deferred until embedding
-- dimensions are pinned per model. See 002_code_tables.sql for rationale.
DROP INDEX IF EXISTS idx_code_embeddings_vector;
