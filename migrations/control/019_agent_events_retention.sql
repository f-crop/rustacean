-- Control schema: RUSAA-1374 — 30-day agent_events retention via partition pruning
--
-- Board decision (2026-05-14): 30-day default retention for agent_events rows,
-- configurable per-tenant. Partition drops are instantaneous and lock-free.
--
-- Design note: agent_events is a shared partitioned table (all tenants share each
-- daily partition). A partition can only be safely dropped when ALL tenants have
-- exceeded their configured retention window for that day. The prune function
-- therefore uses MAX(retention_days) across all active tenants as the global
-- cutoff, guaranteeing that every tenant's data is kept for at least the
-- duration they configured.

-- ---------------------------------------------------------------------------
-- Per-tenant retention configuration
-- ---------------------------------------------------------------------------

ALTER TABLE control.tenants
    ADD COLUMN IF NOT EXISTS agent_events_retention_days INT NOT NULL DEFAULT 30;

ALTER TABLE control.tenants
    DROP CONSTRAINT IF EXISTS tenants_agent_events_retention_days_min;

ALTER TABLE control.tenants
    ADD CONSTRAINT tenants_agent_events_retention_days_min
        CHECK (agent_events_retention_days >= 1);

-- ---------------------------------------------------------------------------
-- agents.seed_agent_events_partition(target_date DATE)
--
-- Idempotently creates the daily partition for target_date if it does not
-- already exist. Safe to call multiple times for the same date.
-- ---------------------------------------------------------------------------

CREATE OR REPLACE FUNCTION agents.seed_agent_events_partition(target_date DATE)
RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    part_name TEXT;
    next_date DATE;
BEGIN
    part_name := 'agent_events_' || to_char(target_date, 'YYYY_MM_DD');
    next_date := target_date + INTERVAL '1 day';

    IF NOT EXISTS (
        SELECT 1
        FROM   pg_class c
        JOIN   pg_namespace n ON n.oid = c.relnamespace
        WHERE  n.nspname = 'agents'
          AND  c.relname = part_name
    ) THEN
        EXECUTE format(
            'CREATE TABLE IF NOT EXISTS agents.%I
             PARTITION OF agents.agent_events
             FOR VALUES FROM (%L) TO (%L)',
            part_name,
            target_date::timestamptz,
            next_date::timestamptz
        );
    END IF;
END;
$$;

-- ---------------------------------------------------------------------------
-- agents.prune_agent_events_partitions()
--
-- Drops daily partitions for agents.agent_events that are older than the
-- maximum retention window configured across all active tenants.
--
-- Returns the number of partitions dropped.
--
-- Idempotent: safe to re-run at any time; re-running after all expired
-- partitions have already been dropped returns 0.
--
-- Skips the default partition (agent_events_default) and any partition
-- whose name does not match the expected YYYY_MM_DD suffix format.
-- ---------------------------------------------------------------------------

CREATE OR REPLACE FUNCTION agents.prune_agent_events_partitions()
RETURNS integer
LANGUAGE plpgsql
AS $$
DECLARE
    max_retention INT;
    cutoff_date   DATE;
    r             RECORD;
    dropped_count INT := 0;
    part_date     DATE;
BEGIN
    -- Conservative cutoff: keep data for all tenants' full retention window.
    -- MAX ensures no tenant has their data deleted before their configured period.
    SELECT COALESCE(MAX(agent_events_retention_days), 30)
    INTO   max_retention
    FROM   control.tenants
    WHERE  status = 'active';

    cutoff_date := current_date - max_retention;

    FOR r IN
        SELECT c.relname AS part_name
        FROM   pg_class     c
        JOIN   pg_namespace n  ON n.oid = c.relnamespace
        JOIN   pg_inherits  i  ON i.inhrelid = c.oid
        JOIN   pg_class     p  ON p.oid = i.inhparent
        JOIN   pg_namespace pn ON pn.oid = p.relnamespace
        WHERE  n.nspname  = 'agents'
          AND  pn.nspname = 'agents'
          AND  p.relname  = 'agent_events'
          AND  c.relname  ~ '^agent_events_\d{4}_\d{2}_\d{2}$'
        ORDER BY c.relname
    LOOP
        BEGIN
            part_date := to_date(
                substring(r.part_name FROM 'agent_events_(\d{4}_\d{2}_\d{2})$'),
                'YYYY_MM_DD'
            );
        EXCEPTION WHEN OTHERS THEN
            RAISE NOTICE 'prune_agent_events_partitions: skipping % (unparseable date)', r.part_name;
            CONTINUE;
        END;

        IF part_date < cutoff_date THEN
            EXECUTE format('DROP TABLE IF EXISTS agents.%I', r.part_name);
            dropped_count := dropped_count + 1;
            RAISE NOTICE 'prune_agent_events_partitions: dropped % (date=%, cutoff=%)',
                r.part_name, part_date, cutoff_date;
        END IF;
    END LOOP;

    RAISE NOTICE 'prune_agent_events_partitions: dropped % partition(s), cutoff=%',
        dropped_count, cutoff_date;
    RETURN dropped_count;
END;
$$;

-- ---------------------------------------------------------------------------
-- Grants
-- ---------------------------------------------------------------------------

GRANT EXECUTE ON FUNCTION agents.seed_agent_events_partition(DATE)
    TO rb_agent_writer;

GRANT EXECUTE ON FUNCTION agents.prune_agent_events_partitions()
    TO rb_agent_writer;
