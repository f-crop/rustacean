-- Per-tenant schema: Phase 4 — Code projection tables
-- Per-ADR-007 §11.9: projector-pg writes to these tables.

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

CREATE INDEX idx_code_files_repo ON code_files (repo_id);
CREATE INDEX idx_code_symbols_file ON code_symbols (file_id);
CREATE INDEX idx_code_symbols_kind ON code_symbols (repo_id, item_kind);
CREATE INDEX idx_code_relations_source ON code_relations (repo_id, source_fqn);
CREATE INDEX idx_code_relations_target ON code_relations (repo_id, target_fqn);
CREATE INDEX idx_code_embeddings_fqn ON code_embeddings (repo_id, item_fqn);
