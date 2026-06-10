-- Control schema: Wave 9 Rewrite — turn_id + parent_user_id on chat events (RUSAA-1973)
-- Additive only; no backfill. Legacy rows have turn_id = NULL, parent_user_id = NULL.
-- AC-4: sessions with only pre-migration rows render identically to today.

ALTER TABLE control.chat_messages
    ADD COLUMN IF NOT EXISTS turn_id        UUID NULL,
    ADD COLUMN IF NOT EXISTS parent_user_id UUID NULL
        REFERENCES control.chat_messages(id) ON DELETE SET NULL;

-- Index for join-by-turn_id in SSE ingest (flush_pending_turn parent_user_id lookup).
CREATE INDEX IF NOT EXISTS chat_messages_turn_id_idx
    ON control.chat_messages(session_id, turn_id)
    WHERE turn_id IS NOT NULL;
