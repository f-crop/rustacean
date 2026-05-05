-- Wave 6 (REQ-TN-04): Efficient index for cancelling in-flight ingestion runs
-- during tenant-wide deletion. The existing idx_ingestion_runs_repo_active is keyed
-- by repo_id; this adds a tenant_id-keyed partial index for the UPDATE query that
-- bulk-cancels all active runs for a tenant being deleted.
CREATE INDEX IF NOT EXISTS idx_ingestion_runs_tenant_active
    ON control.ingestion_runs (tenant_id, status)
    WHERE status IN ('queued', 'running');
