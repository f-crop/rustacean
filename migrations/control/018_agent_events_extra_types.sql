-- Control schema: Wave 12 — Live agent event stream (RUSAA-1315 / RUSAA-1308 Phase 1)
-- Adds session.user_input and session.error to the event_type CHECK constraint so that
-- stream-json runtime events persisted by the new bulk-ingest endpoint are accepted.
-- This migration is additive and backwards-compatible: existing event_type values are
-- unchanged and old control-api binaries never emit the new variants.

ALTER TABLE agents.agent_events
    DROP CONSTRAINT IF EXISTS agent_events_event_type_check;

ALTER TABLE agents.agent_events
    ADD CONSTRAINT agent_events_event_type_check CHECK (
        event_type IN (
            'session.created', 'session.starting', 'session.running',
            'session.tool_call', 'session.tool_result', 'session.message',
            'session.thinking', 'session.paused', 'session.completed', 'session.failed',
            -- new in RUSAA-1315 (stream-json runtime events):
            'session.user_input', 'session.error'
        )
    );
