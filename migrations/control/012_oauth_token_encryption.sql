-- Control schema: RUSAA-862 — KMS key rotation for oauth-claude-v1 (M-2)
--
-- Security audit finding M-2: ADR-009 §7.5 defers KMS key rotation for
-- `oauth-claude-v1` (the key encrypting all Claude OAuth refresh tokens)
-- with no stated SLA.  A key compromise affects all stored tokens until
-- manual intervention.
--
-- This migration adds the `encryption_key_id` column to `agents.oauth_tokens`
-- so each row records which KMS key version encrypted its token columns.
-- The application-level rotation job (see token_key_rotation.rs) uses this
-- column to find and re-encrypt rows that were encrypted with a retired key,
-- or rows that pre-date encryption (value = 'none').
--
-- Rotation procedure (90-day cadence — see ADR-009 §13):
--   1. Generate a new 32-byte hex key and set RB_OAUTH_ENCRYPT_KEY_NEXT=<hex>.
--   2. Promote: set RB_OAUTH_ENCRYPT_KEY=<new>, RB_OAUTH_ENCRYPT_KEY_PREV=<old>,
--               RB_OAUTH_ENCRYPT_KEY_PREV_ID=<old_key_id>.
--   3. Deploy with RB_OAUTH_ROTATE_KEYS_ON_BOOT=true.
--   4. Verify: SELECT encryption_key_id, COUNT(*) FROM agents.oauth_tokens GROUP BY 1;
--   5. Once all rows show the current key id, clear PREV env vars and redeploy.

ALTER TABLE agents.oauth_tokens
    ADD COLUMN IF NOT EXISTS encryption_key_id TEXT NOT NULL DEFAULT 'none';

-- Partial index helps the rotation job find stale-key rows without a
-- full-table scan.  Predicate covers 'none' (plaintext legacy) and any
-- retired key ids; rows already on the current key are excluded.
CREATE INDEX IF NOT EXISTS oauth_tokens_stale_encryption_idx
    ON agents.oauth_tokens (encryption_key_id)
    WHERE encryption_key_id != 'oauth-claude-v1';

-- Grant: rb_oauth_writer already has UPDATE on oauth_tokens (migration 010).
-- No additional grant needed for the new column.
