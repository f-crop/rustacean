-- Control schema: RUSAA-859 — Secure input_prompt in agent_sessions (H-1)
--
-- Security audit finding H-1: ADR-009 §4.1 intended to store input_prompt
-- (full text, 64 KiB cap) in agent_sessions.  Users include credentials and
-- PII in prompts; storing the full text in a persistent row creates an
-- indefinitely-lived plaintext exposure with no declared retention policy.
--
-- Fix (Option 2 + Option 3 from RUSAA-859):
--   - Store only a ≤256-char Unicode preview in the sessions row.
--     The full prompt is forwarded to the runtime adapter but never persisted.
--   - Declare an explicit 90-day retention policy for agent_sessions rows,
--     consistent with the agent_events partition-drop schedule (ADR-009 §4.2).
--     A `purge_old_agent_sessions()` function is provided for cron invocation.
--
-- Merge blocker for RUSAA-84.

-- ---------------------------------------------------------------------------
-- Add input_prompt_preview column
-- ---------------------------------------------------------------------------

ALTER TABLE agents.agent_sessions
    ADD COLUMN IF NOT EXISTS input_prompt_preview TEXT NOT NULL DEFAULT '';

-- Enforce ≤256 Unicode code points at the DB layer as a belt-and-suspenders
-- guard behind the application-level truncation in sessions.rs.
ALTER TABLE agents.agent_sessions
    DROP CONSTRAINT IF EXISTS agent_sessions_prompt_preview_len;

ALTER TABLE agents.agent_sessions
    ADD CONSTRAINT agent_sessions_prompt_preview_len
        CHECK (char_length(input_prompt_preview) <= 256);

-- ---------------------------------------------------------------------------
-- 90-day retention purge function (called by cron / pg_cron)
--
-- Deletes terminal-state sessions whose resolved completion timestamp is
-- older than 90 days.  Mirrors the agent_events partition-drop window.
--
-- Usage (pg_cron example — run nightly at 02:00 UTC):
--   SELECT cron.schedule(
--       'purge-old-agent-sessions',
--       '0 2 * * *',
--       $$SELECT agents.purge_old_agent_sessions()$$
--   );
--
-- Returns the count of deleted rows for logging.
-- ---------------------------------------------------------------------------

CREATE OR REPLACE FUNCTION agents.purge_old_agent_sessions()
RETURNS integer
LANGUAGE plpgsql
AS $$
DECLARE
    deleted_count integer;
BEGIN
    DELETE FROM agents.agent_sessions
    WHERE status IN ('completed', 'failed', 'cancelled')
      AND COALESCE(completed_at, failed_at, created_at)
              < NOW() - INTERVAL '90 days';

    GET DIAGNOSTICS deleted_count = ROW_COUNT;

    RAISE NOTICE 'purge_old_agent_sessions: deleted % row(s)', deleted_count;
    RETURN deleted_count;
END;
$$;

-- Grant execute to the agent writer role so the purge job can run under
-- restricted credentials rather than as superuser.
GRANT EXECUTE ON FUNCTION agents.purge_old_agent_sessions()
    TO rb_agent_writer;
