-- Control schema: Wave 8 S1 — Admin audit log (REQ-AD-01)
-- Creates the auth schema and admin_audit_log table for operator action tracking.
-- Every admin endpoint call writes exactly one row on every code path (invariant §S1.6.1).
-- The auth schema is distinct from the control and audit schemas that already exist.

CREATE SCHEMA IF NOT EXISTS auth;

CREATE TABLE auth.admin_audit_log (
    id              BIGSERIAL    PRIMARY KEY,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    -- Value of X-Admin-Actor header; required on all admin requests.
    actor           TEXT         NOT NULL,
    -- Action identifier, e.g. 'bootstrap.admin', 'tenant.impersonate.start'.
    action          TEXT         NOT NULL,
    -- Nullable for global actions (e.g. bootstrap).
    tenant_id       UUID,
    -- Nullable — set only when a specific user is the target.
    target_user_id  UUID,
    -- The request's X-Request-Id / trace_id (see ADR-012 §S1.4).
    request_id      UUID         NOT NULL,
    ip              INET,
    user_agent      TEXT,
    -- Redacted body summary — must never contain raw secrets (invariant §S1.6.3).
    payload_summary JSONB        NOT NULL,
    outcome         TEXT         NOT NULL CHECK (outcome IN ('ok', 'denied', 'error')),
    error_class     TEXT
);

CREATE INDEX admin_audit_log_tenant_idx  ON auth.admin_audit_log (tenant_id,  created_at DESC);
CREATE INDEX admin_audit_log_actor_idx   ON auth.admin_audit_log (actor,       created_at DESC);
CREATE INDEX admin_audit_log_request_idx ON auth.admin_audit_log (request_id);
