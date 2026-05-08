-- Fix runtime_kind CHECK constraint: 'open_code' → 'opencode'
-- The original migration 010 used 'open_code' but the application inserts 'opencode',
-- causing every opencode session creation to return 500.

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
