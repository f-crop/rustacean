# UAT Fingerprint Contract — Specification

**Status:** Active  
**RUSAA:** [RUSAA-696](/RUSAA/issues/RUSAA-696)  
**Required by:** Gate 3 hook (RUSAA-695)

---

## Overview

A UAT **fingerprint** is a machine-readable record that proves a UAT verdict was produced against a real, live stack — not a mocked or stubbed environment. Gate 3 rejects any PASS verdict that lacks a valid fingerprint.

**A fingerprint produced from a mocked Playwright run is structurally invalid** because:
- `docker inspect` cannot find containers that aren't running
- DB item counts are 0 (no real ingest ran)
- Kafka offsets are 0 (no messages were produced)
- No OpenTelemetry trace IDs exist in `ingestion_runs`

---

## Fingerprint Format

Fingerprints are stored as JSON. The canonical display form uses `pass(...)` / `fail(...)` notation for human readability; the machine form is the raw JSON.

### Display form

```
pass(
  image_shas = {
    control-api:     "sha256:abc123...",
    ingest-clone:    "sha256:def456...",
    expand-worker:   "sha256:789abc...",
    parse-worker:    "sha256:bcd012...",
    typecheck-worker: "sha256:cde234...",
    ingest-graph:    "sha256:ef4567...",
    embed-worker:    "sha256:fab789...",
    projector-pg:    "sha256:012bcd...",
    projector-neo4j: "sha256:123def..."
  },
  db = {
    items:     4218,
    calls:     892,
    relations: 1307
  },
  sse_offsets = {
    rb.ingest.clone.commands:     1,
    rb.ingest.expand.commands:    1,
    rb.ingest.parse.commands:     1,
    rb.parsed-items.v1:           87,
    rb.ingest.typecheck.commands: 1,
    rb.typechecked-items.v1:      87,
    rb.ingest.graph.commands:     1,
    rb.ingest.embed.commands:     1,
    rb.source-files.v1:           12,
    rb.projector.events:          174,
    rb.audit.events:              3
  },
  trace_ids = ["a1b2c3d4e5f6..."]
)
```

### JSON schema

```json
{
  "schema_version": "1",
  "collected_at": "2026-05-07T10:32:00Z",
  "verdict": "pass",
  "image_shas": {
    "control-api":     "sha256:<64-hex>",
    "ingest-clone":    "sha256:<64-hex>",
    "expand-worker":   "sha256:<64-hex>",
    "parse-worker":    "sha256:<64-hex>",
    "typecheck-worker": "sha256:<64-hex>",
    "ingest-graph":    "sha256:<64-hex>",
    "embed-worker":    "sha256:<64-hex>",
    "projector-pg":    "sha256:<64-hex>",
    "projector-neo4j": "sha256:<64-hex>"
  },
  "db": {
    "items":     4218,
    "calls":     892,
    "relations": 1307
  },
  "sse_offsets": {
    "rb.ingest.clone.commands":     1,
    "rb.ingest.expand.commands":    1,
    "rb.ingest.parse.commands":     1,
    "rb.parsed-items.v1":           87,
    "rb.ingest.typecheck.commands": 1,
    "rb.typechecked-items.v1":      87,
    "rb.ingest.graph.commands":     1,
    "rb.ingest.embed.commands":     1,
    "rb.source-files.v1":           12,
    "rb.projector.events":          174,
    "rb.audit.events":              3
  },
  "trace_ids": ["a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4"]
}
```

---

## Field Definitions

### `schema_version`

Version of this spec. Current: `"1"`. Gate 3 rejects unknown versions.

### `verdict`

`"pass"` or `"fail"`. A `"fail"` fingerprint is still written to disk so failures are diffable, but Gate 3 treats it as a blocking gate failure.

### `image_shas`

Per-service image digests collected from the running containers via:

```bash
docker inspect <container_id> --format='{{.Image}}'
```

**Non-negotiable rule:** must come from `docker inspect`, NOT from the compose file image tag. This proves the image actually running matches what was deployed — not just what was declared.

**Validity rule:** every pipeline service must have a non-`MISSING` SHA. Any absent container causes `verdict=fail`.

### `db`

Row counts queried directly from the live databases:

| Field | Source | Query |
|-------|--------|-------|
| `items` | Postgres `<tenant_schema>.code_symbols` | `SELECT COUNT(*) FROM "<schema>".code_symbols` |
| `calls` | Neo4j CALLS relationships | `MATCH ()-[:CALLS]->() RETURN count(*)` |
| `relations` | Neo4j all relationships | `MATCH ()-[r]->() RETURN count(r)` |

**Validity rule:** `items` must be > 0. A zero count means no real ingest completed.

### `sse_offsets`

Kafka topic log-end offsets at the time of collection, gathered from the live broker:

```bash
kafka-get-offsets.sh --bootstrap-server localhost:9092 \
  --topic-partitions 'rb.ingest.clone.commands:0' --time latest
```

The name `sse_offsets` refers to the SSE-visible event stream backed by Kafka. Offsets prove which topics had real message production during the run.

**Validity rule:** at least one topic must have offset > 0. All-zero offsets mean no messages were produced (mocked run).

### `trace_ids`

OpenTelemetry trace IDs from `control.ingestion_runs.trace_id` in Postgres. Collected at the end of the run:

```sql
SELECT trace_id FROM control.ingestion_runs
WHERE trace_id IS NOT NULL
ORDER BY created_at DESC LIMIT 10;
```

Trace IDs are 32-hex strings (128-bit OTel trace format). They prove the request was instrumented by the live OTEL middleware.

**Validity rule:** must contain at least one trace ID. An empty list means OTEL propagation did not reach the live API.

---

## Population Rules (Non-Negotiable)

| Field | Must come from | Must NOT come from |
|-------|----------------|-------------------|
| `image_shas` | `docker inspect <container>` | compose file image tags |
| `db.*` | live Postgres / Neo4j queries | test-seeded fixtures or mocks |
| `sse_offsets` | `kafka-get-offsets.sh` against live broker | stubbed or hardcoded values |
| `trace_ids` | `control.ingestion_runs` in live Postgres | static test data |

---

## Tooling

### Collect fingerprint

```bash
# Default: compose/e2e.yml → /tmp/uat-fingerprint.json
bash scripts/uat-fingerprint.sh

# Custom compose file and output
bash scripts/uat-fingerprint.sh compose/e2e.yml /tmp/my-fingerprint.json

# Scope to a specific ingest run
FINGERPRINT_INGEST_RUN_ID=<uuid> bash scripts/uat-fingerprint.sh
```

### Diff fingerprints

```bash
# Compare candidate against baseline (e.g. main HEAD run)
python3 scripts/fingerprint-diff.py baseline.json candidate.json

# Exit code 0 = identical, 1 = drift detected
```

### Demonstrate mocked-run failure

```bash
# Run against a stack where no containers are running → exits 1
bash scripts/uat-fingerprint.sh compose/e2e.yml /tmp/stub-fingerprint.json
# Expected: FINGERPRINT INVALID (verdict=fail) — exit code 1

# Confirm the diff tool also rejects it
python3 scripts/fingerprint-diff.py /tmp/prev-pass.json /tmp/stub-fingerprint.json
# Expected: RESULT: DRIFT DETECTED — exit code 1
```

---

## Gate 3 Requirements (RUSAA-695)

Once Gate 3 hook is live, a PASS verdict on any Paperclip issue is rejected unless:

1. A `deployment-fingerprint` document exists on the issue.
2. `verdict` is `"pass"`.
3. `image_shas` matches the images built from the merged commit SHA (no drift from `main` HEAD).
4. `db.items` > 0.
5. At least one `sse_offsets` value > 0.
6. `trace_ids` is non-empty.

---

## Live-Stack UAT Requirement

Mocked Playwright tests (`page.route(...)` stubs) structurally cannot produce a valid fingerprint. The standard test suite in `frontend/e2e/` uses mocked routes and therefore cannot satisfy Gate 3.

To satisfy Gate 3, at least one UAT must:
1. Run against the real compose stack (no `page.route()` mocks on API endpoints)
2. Complete successfully while `uat-fingerprint.sh` is collecting data from the same stack
3. Produce a `verdict=pass` fingerprint

See `frontend/e2e/ingestion-live.spec.ts` for the live-stack reference implementation.
