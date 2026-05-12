-- Control schema: GH-XX Phase 1 — Self-service GitHub App registration via Manifest flow.
--
-- Adds the storage half of the Manifest flow: a singleton-active
-- `github_app_config` row that holds the App credentials (encrypted), a
-- short-lived `github_manifest_states` table for replay-safe state tokens,
-- and a `users.is_platform_admin` flag gating the admin-only register flow.
--
-- The actual register endpoints, per-request GhApp loader, and FE screen
-- land in subsequent phases. After this migration applies, the env-var path
-- in `services/control-api/src/server.rs::build_gh_app` continues to work
-- unchanged.
--
-- Encryption: AES-256-GCM with per-row 12-byte random nonce.  Key comes
-- from `RB_GH_APP_ENC_KEY` (base64 32 bytes), dedicated and independent
-- from `RB_TOKEN_ENC_KEY` so GH App secret rotation does not require
-- re-encrypting OAuth tokens (and vice versa).  `encryption_key_id`
-- defaults to `'gh-app-v1'` and is the rotation pivot.
--
-- Migration type: Additive (CREATE TABLE, ADD COLUMN with default).
-- Joint approval: Platform + Architect per AGENTS.md § Schema Migrations.

ALTER TABLE users
    ADD COLUMN is_platform_admin BOOLEAN NOT NULL DEFAULT false;

-- App credentials.  Singleton-active is enforced by the partial unique
-- index below: at most one row may have is_active=true at any time.
-- Replacing the active App is a two-statement transaction:
--   UPDATE github_app_config SET is_active=false, deactivated_at=now() WHERE is_active;
--   INSERT INTO github_app_config (...) VALUES (..., true);
CREATE TABLE github_app_config (
    id                          BIGSERIAL   PRIMARY KEY,
    app_id                      BIGINT      NOT NULL,
    slug                        TEXT        NOT NULL,
    client_id                   TEXT        NOT NULL,
    client_secret_ciphertext    BYTEA       NOT NULL,
    client_secret_nonce         BYTEA       NOT NULL,
    private_key_ciphertext      BYTEA       NOT NULL,
    private_key_nonce           BYTEA       NOT NULL,
    webhook_secret_ciphertext   BYTEA       NOT NULL,
    webhook_secret_nonce        BYTEA       NOT NULL,
    encryption_key_id           TEXT        NOT NULL DEFAULT 'gh-app-v1',
    installed_by_user_id        UUID        NOT NULL REFERENCES users(id),
    is_active                   BOOLEAN     NOT NULL DEFAULT true,
    created_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    deactivated_at              TIMESTAMPTZ
);

-- At most one row may be active at any time.  Postgres ((1)) expression
-- index is a documented pattern for whole-table singletons since PG14.
CREATE UNIQUE INDEX github_app_config_singleton_active_idx
    ON github_app_config ((1)) WHERE is_active;

-- Replacement / audit queries scan recent rows first.
CREATE INDEX github_app_config_created_at_idx
    ON github_app_config (created_at DESC);

-- One-shot state tokens for the manifest exchange.  Mirrors
-- github_install_states: hashed token, 10-minute TTL, consumed_at for
-- replay protection.
CREATE TABLE github_manifest_states (
    id                   BIGSERIAL   PRIMARY KEY,
    state_token_hash     BYTEA       NOT NULL,
    initiated_by_user_id UUID        NOT NULL REFERENCES users(id),
    expires_at           TIMESTAMPTZ NOT NULL,
    consumed_at          TIMESTAMPTZ,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX github_manifest_states_hash_idx
    ON github_manifest_states (state_token_hash);

-- Sweeper-friendly index: open (un-consumed) tokens ordered by expiry.
CREATE INDEX github_manifest_states_active_idx
    ON github_manifest_states (expires_at) WHERE consumed_at IS NULL;
