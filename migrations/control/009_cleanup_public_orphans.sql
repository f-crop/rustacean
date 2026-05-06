-- Remove application tables accidentally placed in public during RUSAA-668 manual migration repair.
-- These four tables belong only in per-tenant schemas (tenant_XXXX); public holds only extensions.
DROP TABLE IF EXISTS public.code_embeddings;
DROP TABLE IF EXISTS public.code_relations;
DROP TABLE IF EXISTS public.code_symbols;
DROP TABLE IF EXISTS public.code_files;

-- Ensure the pgvector extension is anchored in the shared public schema.
-- Migration 002_code_tables ran CREATE EXTENSION IF NOT EXISTS vector with search_path set to
-- the tenant schema first, so the extension objects landed there instead of public.
-- This DO block handles three cases:
--   * not installed yet   → install in public
--   * installed elsewhere → move to public (ALTER EXTENSION SET SCHEMA)
--   * already in public   → no-op
DO $$
DECLARE
    ext_schema text;
BEGIN
    SELECT n.nspname INTO ext_schema
    FROM pg_extension e
    JOIN pg_namespace n ON n.oid = e.extnamespace
    WHERE e.extname = 'vector';

    IF ext_schema IS NULL THEN
        EXECUTE 'CREATE EXTENSION vector SCHEMA public';
    ELSIF ext_schema <> 'public' THEN
        EXECUTE 'ALTER EXTENSION vector SET SCHEMA public';
    END IF;
END $$;
