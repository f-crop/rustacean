-- Wave 5 (ADR-007 §11.9): per-tenant code projection tables.
-- Written by projector-pg; read by control-api search endpoints.

-- pgvector is required for the code_embeddings table (vector column type).
CREATE EXTENSION IF NOT EXISTS vector;

-- Source files discovered during clone stage.
-- Idempotency: (repo_id, relative_path) is UNIQUE.
CREATE TABLE code_files (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id         UUID        NOT NULL,
    relative_path   TEXT        NOT NULL,
    sha256          TEXT        NOT NULL,  -- hex digest, 64 chars
    size_bytes      BIGINT      NOT NULL,
    blob_ref        TEXT,               -- rb-blob:// pointer if large
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (repo_id, relative_path)
);

CREATE INDEX idx_code_files_repo ON code_files (repo_id);
CREATE INDEX idx_code_files_sha256 ON code_files (sha256);

-- Code symbols (functions, structs, enums, traits, etc.) extracted by parser.
-- Idempotency: (repo_id, fqn) is UNIQUE per ADR-007 §11.9.
CREATE TABLE code_symbols (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id         UUID        NOT NULL,
    fqn             TEXT        NOT NULL,  -- fully-qualified name
    kind            TEXT        NOT NULL,  -- ItemKind variant
    source_path     TEXT,               -- relative path in repo
    line_start      INTEGER,
    line_end        INTEGER,
    blob_ref        TEXT,               -- rb-blob:// pointer for AST JSON
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (repo_id, fqn)
);

CREATE INDEX idx_code_symbols_repo ON code_symbols (repo_id);
CREATE INDEX idx_code_symbols_kind ON code_symbols (kind);

-- Relations between symbols (calls, implements, uses, etc.).
-- Idempotency: (repo_id, from_fqn, to_fqn, kind) is UNIQUE.
CREATE TABLE code_relations (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id         UUID        NOT NULL,
    from_fqn        TEXT        NOT NULL,
    to_fqn          TEXT        NOT NULL,
    kind            TEXT        NOT NULL,  -- RelationKind variant
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (repo_id, from_fqn, to_fqn, kind)
);

CREATE INDEX idx_code_relations_repo ON code_relations (repo_id);
CREATE INDEX idx_code_relations_from ON code_relations (from_fqn);
CREATE INDEX idx_code_relations_to ON code_relations (to_fqn);

-- Vector embeddings for semantic search.
-- One row per (symbol_fqn, model) pair.
CREATE TABLE code_embeddings (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id         UUID        NOT NULL,
    symbol_fqn      TEXT        NOT NULL,
    embedding_model TEXT        NOT NULL,  -- e.g. "openai/text-embedding-3-small"
    dimensions      INTEGER     NOT NULL,
    vector          vector,              -- pgvector extension
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (repo_id, symbol_fqn, embedding_model)
);

CREATE INDEX idx_code_embeddings_repo ON code_embeddings (repo_id);
-- Vector similarity index omitted: pgvector requires explicit column dimensions for
-- ivfflat/hnsw but this column is intentionally dimensionless (stores multiple models).
-- Add a partial index per model dimension in a later migration.
