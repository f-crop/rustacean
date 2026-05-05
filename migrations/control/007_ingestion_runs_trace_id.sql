-- REQ-FE-08 (ADR-008 §12.11 AC4): add trace_id to ingestion_runs for trace viewer reverse-lookup.
-- trace_id is a 32-hex OpenTelemetry trace ID propagated from the ingest command envelope.
ALTER TABLE ingestion_runs ADD COLUMN trace_id TEXT;

CREATE INDEX idx_ingestion_runs_trace_id
    ON ingestion_runs (trace_id)
    WHERE trace_id IS NOT NULL;
