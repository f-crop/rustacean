-- DROP orphan tenant_*.agent_sessions tables (destructive).
--
-- Board approval: Gate-2 approval 704c9bb8-7816-43d0-82da-8e79548bc39c.
-- Source of truth for agent sessions: agents.agent_sessions in the control
-- schema (ADR-009 §4.1). Prior investigation confirmed the tenant-scoped
-- copies of this table are dead state (0 rows across all schemas) with no
-- code readers or writers — they were left behind by a reverted ad-hoc
-- provisioning script and were never integrated into the tenant template.
--
-- For each tenant_* schema that still has an orphan agent_sessions table, drop:
--   1. trigger     update_agent_sessions_updated_at_trigger
--   2. table       agent_sessions
--   3. function    update_agent_sessions_updated_at()
--
-- Idempotent: uses DROP ... IF EXISTS and conditional schema-table lookup,
-- so re-running is a no-op. Scope adjusts dynamically to current tenant count.

DO $$
DECLARE
    s_name TEXT;
BEGIN
    FOR s_name IN
        SELECT n.nspname
        FROM pg_namespace n
        WHERE n.nspname LIKE 'tenant\_%' ESCAPE '\'
        ORDER BY n.nspname
    LOOP
        IF EXISTS (
            SELECT 1
            FROM pg_class c
            JOIN pg_namespace ns ON ns.oid = c.relnamespace
            WHERE ns.nspname = s_name
              AND c.relname  = 'agent_sessions'
              AND c.relkind  = 'r'
        ) THEN
            EXECUTE format(
                'DROP TRIGGER IF EXISTS update_agent_sessions_updated_at_trigger ON %I.agent_sessions',
                s_name
            );
            EXECUTE format('DROP TABLE IF EXISTS %I.agent_sessions', s_name);
            EXECUTE format(
                'DROP FUNCTION IF EXISTS %I.update_agent_sessions_updated_at()',
                s_name
            );
            RAISE NOTICE 'dropped orphan agent_sessions in schema %', s_name;
        END IF;
    END LOOP;
END $$;
