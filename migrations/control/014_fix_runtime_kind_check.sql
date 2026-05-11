-- Control schema: RUSAA-1068 — fix runtime_kind CHECK ('open_code' → 'opencode', + 'pi')
--
-- Drift recovery: this file was previously named 012_fix_runtime_kind_check.sql,
-- but live DBs already had a v12 entry recorded for the unrelated migration
-- 012_oauth_token_encryption (checksum 81e4d311…), so the migrator skipped this
-- file every boot.  Renumbering to 014 lets it apply on top of the restored
-- 012/013 sequence without colliding with applied state.
--
-- Effect: relaxes the runtime_kind CHECK to admit the values the application
-- actually inserts ('opencode' instead of 'open_code', plus 'pi').  Before this
-- runs, POST /v1/agents/sessions {"runtime":"opencode"} returns 500 from the
-- DB-level constraint violation.

ALTER TABLE agents.agent_sessions
    DROP CONSTRAINT IF EXISTS agent_sessions_runtime_kind_check;

-- Update any existing rows that may have been stored as 'open_code' (unlikely in
-- practice since inserts were failing, but safe to do for schema consistency).
UPDATE agents.agent_sessions
    SET runtime_kind = 'opencode'
    WHERE runtime_kind = 'open_code';

ALTER TABLE agents.agent_sessions
    ADD CONSTRAINT agent_sessions_runtime_kind_check
        CHECK (runtime_kind IN ('claude_code', 'opencode', 'pi'));
