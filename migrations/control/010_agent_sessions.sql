-- Control schema: Wave 7 — Agent execution backend (RUSAA-84 / ADR-009 Phase 1)
-- Tables: agent_sessions, agent_events (range-partitioned), oauth_tokens
-- Roles: rb_agent_writer, rb_oauth_writer
-- Constraint: tenant_id immutability trigger on agent_sessions

-- ---------------------------------------------------------------------------
-- Schema
-- ---------------------------------------------------------------------------

CREATE SCHEMA IF NOT EXISTS agents;

-- ---------------------------------------------------------------------------
-- Roles
-- ---------------------------------------------------------------------------

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'rb_agent_writer') THEN
        CREATE ROLE rb_agent_writer NOLOGIN;
    END IF;
END $$;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'rb_oauth_writer') THEN
        CREATE ROLE rb_oauth_writer NOLOGIN;
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- agent_sessions
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS agents.agent_sessions (
    id              UUID        NOT NULL PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id       UUID        NOT NULL,
    user_id         UUID        NOT NULL,
    runtime_kind    TEXT        NOT NULL,  -- 'claude_code' | 'open_code' | 'pi'
    model           TEXT        NOT NULL,
    system_prompt   TEXT        NOT NULL DEFAULT '',
    status          TEXT        NOT NULL DEFAULT 'created',
    -- 'created' | 'starting' | 'running' | 'paused' | 'completed' | 'failed' | 'cancelled'
    token_budget    BIGINT      NOT NULL DEFAULT 100000,
    tokens_used     BIGINT      NOT NULL DEFAULT 0,
    metadata        JSONB       NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at      TIMESTAMPTZ,
    completed_at    TIMESTAMPTZ,
    failed_at       TIMESTAMPTZ,
    failure_reason  TEXT,
    CONSTRAINT agent_sessions_runtime_kind_check
        CHECK (runtime_kind IN ('claude_code', 'open_code', 'pi')),
    CONSTRAINT agent_sessions_status_check
        CHECK (status IN ('created', 'starting', 'running', 'paused', 'completed', 'failed', 'cancelled')),
    CONSTRAINT agent_sessions_token_budget_positive CHECK (token_budget > 0)
);

-- Per-tenant listing (primary access pattern).
CREATE INDEX IF NOT EXISTS agent_sessions_tenant_created_idx
    ON agents.agent_sessions (tenant_id, created_at DESC);

-- Status filter for active-session registry queries.
CREATE INDEX IF NOT EXISTS agent_sessions_tenant_status_idx
    ON agents.agent_sessions (tenant_id, status)
    WHERE status NOT IN ('completed', 'failed', 'cancelled');

-- ---------------------------------------------------------------------------
-- Tenant-id immutability trigger on agent_sessions
-- ---------------------------------------------------------------------------

CREATE OR REPLACE FUNCTION agents.prevent_tenant_id_change()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.tenant_id <> OLD.tenant_id THEN
        RAISE EXCEPTION 'tenant_id is immutable on agent_sessions (old=%, new=%)',
            OLD.tenant_id, NEW.tenant_id
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS agent_sessions_tenant_immutable ON agents.agent_sessions;
CREATE TRIGGER agent_sessions_tenant_immutable
    BEFORE UPDATE ON agents.agent_sessions
    FOR EACH ROW EXECUTE FUNCTION agents.prevent_tenant_id_change();

-- ---------------------------------------------------------------------------
-- agent_events  (range-partitioned by created_at — daily buckets)
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS agents.agent_events (
    id              UUID        NOT NULL DEFAULT gen_random_uuid(),
    session_id      UUID        NOT NULL REFERENCES agents.agent_sessions(id) ON DELETE CASCADE,
    tenant_id       UUID        NOT NULL,
    event_type      TEXT        NOT NULL,
    -- 'session.created' | 'session.starting' | 'session.running'
    -- | 'session.tool_call' | 'session.tool_result' | 'session.message'
    -- | 'session.thinking' | 'session.paused' | 'session.completed' | 'session.failed'
    sequence        BIGINT      NOT NULL,
    payload         JSONB       NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id, created_at),
    CONSTRAINT agent_events_event_type_check CHECK (
        event_type IN (
            'session.created', 'session.starting', 'session.running',
            'session.tool_call', 'session.tool_result', 'session.message',
            'session.thinking', 'session.paused', 'session.completed', 'session.failed'
        )
    )
) PARTITION BY RANGE (created_at);

-- Default partition catches events outside explicit day partitions.
CREATE TABLE IF NOT EXISTS agents.agent_events_default
    PARTITION OF agents.agent_events DEFAULT;

-- Seed today and tomorrow so new sessions land in an indexed partition.
DO $$
DECLARE
    today   DATE := current_date;
    tomorrow DATE := current_date + 1;
    part_name TEXT;
BEGIN
    part_name := 'agent_events_' || to_char(today, 'YYYY_MM_DD');
    IF NOT EXISTS (
        SELECT 1 FROM pg_class c
        JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname = 'agents' AND c.relname = part_name
    ) THEN
        EXECUTE format(
            'CREATE TABLE IF NOT EXISTS agents.%I PARTITION OF agents.agent_events
             FOR VALUES FROM (%L) TO (%L)',
            part_name, today::timestamptz, tomorrow::timestamptz
        );
    END IF;
END $$;

-- Per-session ordered replay (primary access pattern for SSE events stream).
CREATE INDEX IF NOT EXISTS agent_events_session_seq_idx
    ON agents.agent_events (session_id, sequence ASC);

-- Per-tenant listing.
CREATE INDEX IF NOT EXISTS agent_events_tenant_created_idx
    ON agents.agent_events (tenant_id, created_at DESC);

-- ---------------------------------------------------------------------------
-- oauth_tokens
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS agents.oauth_tokens (
    id              UUID        NOT NULL PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id       UUID        NOT NULL,
    user_id         UUID        NOT NULL,
    provider        TEXT        NOT NULL,  -- 'claude_code' | 'open_code' | 'pi'
    -- Tokens stored as hex-encoded AES-256-GCM ciphertext (key = RB_OAUTH_ENCRYPT_KEY).
    access_token    TEXT        NOT NULL,
    refresh_token   TEXT,
    expires_at      TIMESTAMPTZ,
    scopes          TEXT[]      NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT oauth_tokens_provider_check
        CHECK (provider IN ('claude_code', 'open_code', 'pi')),
    -- One active token per (tenant, user, provider).
    CONSTRAINT oauth_tokens_tenant_user_provider_uidx
        UNIQUE (tenant_id, user_id, provider)
);

CREATE INDEX IF NOT EXISTS oauth_tokens_tenant_user_idx
    ON agents.oauth_tokens (tenant_id, user_id);

-- ---------------------------------------------------------------------------
-- Grants
-- ---------------------------------------------------------------------------

GRANT USAGE ON SCHEMA agents TO rb_agent_writer;
GRANT INSERT, SELECT, UPDATE ON agents.agent_sessions TO rb_agent_writer;
GRANT INSERT, SELECT ON agents.agent_events TO rb_agent_writer;

GRANT USAGE ON SCHEMA agents TO rb_oauth_writer;
GRANT INSERT, SELECT, UPDATE, DELETE ON agents.oauth_tokens TO rb_oauth_writer;
