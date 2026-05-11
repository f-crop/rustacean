-- Control schema: #351 — add 'terminating' status to agent_sessions
--
-- DELETE /v1/agents/sessions/{id} now sets status = 'terminating' immediately
-- so the session does not appear "pending" while awaiting agent-runner pickup.
-- agent-runner overwrites this with 'terminated' once the process exits.

ALTER TABLE agents.agent_sessions
    DROP CONSTRAINT IF EXISTS agent_sessions_status_check;

ALTER TABLE agents.agent_sessions
    ADD CONSTRAINT agent_sessions_status_check
        CHECK (status IN (
            'created', 'starting', 'running', 'paused',
            'completed', 'failed', 'cancelled',
            'pending', 'terminating', 'terminated'
        ));
