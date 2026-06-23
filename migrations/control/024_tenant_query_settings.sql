-- Wave 10 S5: per-tenant multi-query rewrite settings (ADR-014 §6).
--
-- Stores the per-tenant override for multi-query expansion:
--   multi_query_n          — number of query variants (1 = off, max 3).
--   multi_query_force_off  — tenant can hard-disable expansion even when global n > 1.
--   llm_token_budget       — per-call token ceiling for the rewrite prompt; 0 = disabled.
--
-- Rows are optional: absence means "use global defaults" (n=1, force_off=false, budget=0).
-- ON DELETE CASCADE: tenant deletion sweeps this table automatically (REQ-TN-04).

CREATE TABLE IF NOT EXISTS control.tenant_query_settings (
    tenant_id             UUID        PRIMARY KEY
                                      REFERENCES control.tenants(id) ON DELETE CASCADE,
    multi_query_n         SMALLINT    NOT NULL DEFAULT 1
                                      CHECK (multi_query_n BETWEEN 1 AND 3),
    multi_query_force_off BOOLEAN     NOT NULL DEFAULT false,
    llm_token_budget      INT         NOT NULL DEFAULT 0
                                      CHECK (llm_token_budget >= 0),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

COMMENT ON TABLE control.tenant_query_settings IS
    'Per-tenant overrides for Wave 10 S5 multi-query rewrite. Absence = global defaults.';
