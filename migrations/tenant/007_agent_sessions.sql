-- Per-tenant schema: RUSAA-1096 — agent_sessions provisioning
--
-- Tenant-scoped table tracking process-spawning agent sessions per ADR-009
-- Option B.  Distinct from public/shared agents.agent_sessions; this table
-- records per-tenant lifecycle metadata for the agent-runner.
--
-- 22 existing tenant schemas already carry this table from a one-shot manual
-- backfill (recorded with version 7, checksum 091d969b…).  This file is the
-- canonical source going forward so that:
--   - migrate-tenant.sh (rb-migrations init container) applies it to the
--     three older tenants that missed the backfill (startup self-heal),
--   - the Rust signup flow applies it to every new tenant immediately,
--   - all CREATE statements are idempotent (IF NOT EXISTS / OR REPLACE /
--     DROP TRIGGER IF EXISTS) so re-application is safe.
--
-- Coordinates with Wave 7 control-schema migrations 010/013/014.  This file
-- does NOT alter agents.agent_sessions (the global table); the fix is
-- tenant-schema-scoped per the parent RUSAA-1071 acceptance note.

CREATE TABLE IF NOT EXISTS agent_sessions (
    id                  UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id           UUID        NOT NULL,
    session_id          TEXT        NOT NULL UNIQUE,
    runtime             TEXT        NOT NULL,
    status              TEXT        NOT NULL,
    workspace_path      TEXT        NOT NULL,
    api_key_id          UUID,
    pid                 INTEGER,
    exit_code           INTEGER,
    duration_ms         BIGINT,
    termination_reason  TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    terminated_at       TIMESTAMPTZ,
    CONSTRAINT agent_sessions_runtime_check
        CHECK (runtime IN ('claude_code', 'opencode', 'pi')),
    CONSTRAINT agent_sessions_status_check
        CHECK (status IN ('pending', 'running', 'terminated', 'error'))
);

CREATE INDEX IF NOT EXISTS idx_agent_sessions_tenant_id  ON agent_sessions (tenant_id);
CREATE INDEX IF NOT EXISTS idx_agent_sessions_session_id ON agent_sessions (session_id);
CREATE INDEX IF NOT EXISTS idx_agent_sessions_status     ON agent_sessions (status);

-- updated_at maintenance trigger — function lives in the tenant schema so
-- each tenant owns its own copy (matches existing 22-tenant backfill layout).
CREATE OR REPLACE FUNCTION update_agent_sessions_updated_at()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS update_agent_sessions_updated_at_trigger ON agent_sessions;
CREATE TRIGGER update_agent_sessions_updated_at_trigger
    BEFORE UPDATE ON agent_sessions
    FOR EACH ROW EXECUTE FUNCTION update_agent_sessions_updated_at();
