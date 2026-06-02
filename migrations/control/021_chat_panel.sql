-- Control schema: Wave 9 S3 — Chat panel persistence (ADR-013 §7)
-- Adds chat_sessions and chat_messages tables.
-- Additive only; no backfill.
-- tenant_id FK ON DELETE CASCADE ensures tenant deletion sweeps chat data (REQ-TN-04).

CREATE TABLE control.chat_sessions (
    id               UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id        UUID        NOT NULL REFERENCES control.tenants(id) ON DELETE CASCADE,
    user_id          UUID        REFERENCES control.users(id) ON DELETE SET NULL,
    runtime          TEXT        NOT NULL CHECK (runtime IN ('claude_code','opencode','pi')),
    status           TEXT        NOT NULL DEFAULT 'active'
                     CHECK (status IN ('active','ended','failed')),
    trace_id         TEXT        NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_activity_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at         TIMESTAMPTZ
);

CREATE INDEX chat_sessions_tenant_created_idx
    ON control.chat_sessions(tenant_id, created_at DESC);
CREATE INDEX chat_sessions_idle_idx
    ON control.chat_sessions(status, last_activity_at)
    WHERE status = 'active';

-- Immutability trigger mirrors agent_sessions pattern (ADR-013 §7).
CREATE OR REPLACE FUNCTION control.chat_sessions_tenant_id_immutable()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.tenant_id IS DISTINCT FROM OLD.tenant_id THEN
        RAISE EXCEPTION 'chat_sessions.tenant_id is immutable (was %, attempted %)',
            OLD.tenant_id, NEW.tenant_id USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER chat_sessions_tenant_id_immutable_trg
BEFORE UPDATE ON control.chat_sessions
FOR EACH ROW EXECUTE FUNCTION control.chat_sessions_tenant_id_immutable();

CREATE TABLE control.chat_messages (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id  UUID        NOT NULL REFERENCES control.chat_sessions(id) ON DELETE CASCADE,
    tenant_id   UUID        NOT NULL REFERENCES control.tenants(id) ON DELETE CASCADE,
    seq         INTEGER     NOT NULL,
    role        TEXT        NOT NULL CHECK (role IN ('user','assistant','system','tool')),
    body        TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (session_id, seq)
);

CREATE INDEX chat_messages_session_seq_idx ON control.chat_messages(session_id, seq);

-- 90-day retention purge function (mirrors agent_sessions pattern).
-- Invoked by a cron job (0 2 * * * UTC).
CREATE OR REPLACE FUNCTION control.purge_old_chat_sessions()
RETURNS void LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM control.chat_sessions
    WHERE status IN ('ended', 'failed')
      AND ended_at < now() - INTERVAL '90 days';
END $$;
