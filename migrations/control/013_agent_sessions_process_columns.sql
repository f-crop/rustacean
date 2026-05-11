-- Control schema: RUSAA-1067 — restore process-spawning columns on agent_sessions
--
-- Drift recovery for PR #319 (commit 220c351), which mutated the already-applied
-- migration 010 to add four columns (pid, exit_code, workspace_path, api_key_id)
-- and expanded the status CHECK constraint.  Because v10 was already in
-- control.schema_migrations (checksum eb8d1f07…), the migrator skipped it on
-- subsequent boots, so the column additions never landed in the live schema and
-- POST /v1/agents/sessions returned 500 with
-- 'column "workspace_path" of relation "agent_sessions" does not exist'.
--
-- Source migration 010 has been reverted to its pre-PR-#319 content so its
-- checksum matches the recorded value.  This forward migration carries the
-- additive changes that 010 was attempting to make.

ALTER TABLE agents.agent_sessions
    ADD COLUMN IF NOT EXISTS pid             INTEGER,
    ADD COLUMN IF NOT EXISTS exit_code       INTEGER,
    ADD COLUMN IF NOT EXISTS workspace_path  TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS api_key_id      UUID;

-- ADR-009 Option B introduces two additional session lifecycle states:
--   'pending'    — spawned, waiting for runtime adapter to take ownership
--   'terminated' — process exited or was killed before completion
-- Replace the CHECK constraint to admit both.

ALTER TABLE agents.agent_sessions
    DROP CONSTRAINT IF EXISTS agent_sessions_status_check;

ALTER TABLE agents.agent_sessions
    ADD CONSTRAINT agent_sessions_status_check
        CHECK (status IN (
            'created', 'starting', 'running', 'paused',
            'completed', 'failed', 'cancelled',
            'pending', 'terminated'
        ));
