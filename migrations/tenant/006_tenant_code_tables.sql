-- Per-tenant schema: Phase 4 — Code projection tables
-- Per-ADR-007 §11.9: projector-pg writes to these tables.
--
-- NOTE: code_files/code_symbols/code_relations/code_embeddings may already
-- exist from migration 002 (Wave 5 schema). CREATE TABLE IF NOT EXISTS skips
-- creation when tables exist; index creation is guarded via DO blocks so it
-- only runs when the expected columns are present (Phase 4 schema).

CREATE TABLE IF NOT EXISTS code_files (
    id              UUID        PRIMARY KEY,
    repo_id         UUID        NOT NULL,
    relative_path   TEXT        NOT NULL,
    sha256          TEXT        NOT NULL,
    size_bytes      BIGINT      NOT NULL,
    ingest_run_id   UUID,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (repo_id, relative_path)
);

CREATE TABLE IF NOT EXISTS code_symbols (
    id              UUID        PRIMARY KEY,
    repo_id         UUID        NOT NULL,
    file_id         UUID        NOT NULL REFERENCES code_files(id) ON DELETE CASCADE,
    fqn             TEXT        NOT NULL,
    item_kind       TEXT        NOT NULL,
    item_data       JSONB,
    ingest_run_id   UUID,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (repo_id, fqn)
);

CREATE TABLE IF NOT EXISTS code_relations (
    id              UUID        PRIMARY KEY,
    repo_id         UUID        NOT NULL,
    source_fqn      TEXT        NOT NULL,
    target_fqn      TEXT        NOT NULL,
    relation_kind   TEXT        NOT NULL,
    relation_data   JSONB,
    ingest_run_id   UUID,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS code_embeddings (
    id              UUID        PRIMARY KEY,
    repo_id         UUID        NOT NULL,
    item_fqn        TEXT        NOT NULL,
    model_id        TEXT        NOT NULL,
    embedding       BYTEA,
    ingest_run_id   UUID,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (repo_id, item_fqn, model_id)
);

-- repo_id index is safe on any schema version.
CREATE INDEX IF NOT EXISTS idx_code_files_repo ON code_files (repo_id);

-- Phase 4 indexes only — guard on column existence to handle Wave 5 schema.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name   = 'code_symbols'
          AND column_name  = 'file_id'
    ) THEN
        IF NOT EXISTS (
            SELECT 1 FROM pg_indexes
            WHERE schemaname = current_schema() AND indexname = 'idx_code_symbols_file'
        ) THEN
            CREATE INDEX idx_code_symbols_file ON code_symbols (file_id);
        END IF;
    END IF;

    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name   = 'code_symbols'
          AND column_name  = 'item_kind'
    ) THEN
        IF NOT EXISTS (
            SELECT 1 FROM pg_indexes
            WHERE schemaname = current_schema() AND indexname = 'idx_code_symbols_kind'
        ) THEN
            CREATE INDEX idx_code_symbols_kind ON code_symbols (repo_id, item_kind);
        END IF;
    END IF;

    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name   = 'code_relations'
          AND column_name  = 'source_fqn'
    ) THEN
        IF NOT EXISTS (
            SELECT 1 FROM pg_indexes
            WHERE schemaname = current_schema() AND indexname = 'idx_code_relations_source'
        ) THEN
            CREATE INDEX idx_code_relations_source ON code_relations (repo_id, source_fqn);
        END IF;
    END IF;

    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name   = 'code_relations'
          AND column_name  = 'target_fqn'
    ) THEN
        IF NOT EXISTS (
            SELECT 1 FROM pg_indexes
            WHERE schemaname = current_schema() AND indexname = 'idx_code_relations_target'
        ) THEN
            CREATE INDEX idx_code_relations_target ON code_relations (repo_id, target_fqn);
        END IF;
    END IF;

    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name   = 'code_embeddings'
          AND column_name  = 'item_fqn'
    ) THEN
        IF NOT EXISTS (
            SELECT 1 FROM pg_indexes
            WHERE schemaname = current_schema() AND indexname = 'idx_code_embeddings_fqn'
        ) THEN
            CREATE INDEX idx_code_embeddings_fqn ON code_embeddings (repo_id, item_fqn);
        END IF;
    END IF;
END
$$;
