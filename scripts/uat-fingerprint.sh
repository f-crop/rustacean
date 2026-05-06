#!/usr/bin/env bash
# UAT Fingerprint Collector (RUSAA-696 — Pillar C Phase 2)
#
# Collects a cryptographic fingerprint from the running live stack and writes a
# structured JSON verdict. A fingerprint produced from a mocked/stubbed stack
# will FAIL because:
#   - docker inspect finds no running containers (image_shas empty)
#   - DB item count is 0 (no real ingest ran against live Postgres)
#   - All Kafka topic offsets are 0 (no messages produced)
#   - No trace_ids exist in ingestion_runs
#
# Required fields (all must be non-empty/non-zero for a PASS):
#   image_shas  — per-service image digests from `docker inspect`, NOT compose file
#   db          — row counts queried directly from live Postgres + Neo4j
#   sse_offsets — Kafka topic log-end offsets from the live broker
#   trace_ids   — OpenTelemetry trace IDs from control.ingestion_runs
#
# Usage:
#   bash scripts/uat-fingerprint.sh [COMPOSE_FILE] [OUTPUT_FILE]
#
#   COMPOSE_FILE  — docker compose file (default: compose/e2e.yml)
#   OUTPUT_FILE   — fingerprint JSON output path (default: /tmp/uat-fingerprint.json)
#
# Optional env vars:
#   FINGERPRINT_INGEST_RUN_ID   — scope trace_id query to a specific run
#   FINGERPRINT_TENANT_SCHEMA   — scope item count to a specific tenant schema
#
# Exit codes:
#   0 — fingerprint valid; all required fields populated from live data
#   1 — one or more required fields could not be populated (fingerprint is invalid)

set -euo pipefail

COMPOSE_FILE="${1:-compose/e2e.yml}"
OUTPUT_FILE="${2:-/tmp/uat-fingerprint.json}"

DC="docker compose -f ${COMPOSE_FILE}"
TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
VERDICT_FAILED=0

log()        { echo "[$(date -u +%H:%M:%S)] [fingerprint] $*"; }
warn()       { echo "[$(date -u +%H:%M:%S)] [fingerprint] WARN: $*" >&2; }
fail_field() { VERDICT_FAILED=1; warn "INVALID: $*"; }

# ── Helpers ───────────────────────────────────────────────────────────────────

psql_q() {
  ${DC} exec -T postgres psql -U rustbrain -d rustbrain -t -A -c "$1" 2>/dev/null
}

neo4j_q() {
  ${DC} exec -T neo4j cypher-shell \
    -u neo4j -p rustbrain123 --format plain "$1" 2>/dev/null \
    | tail -1 | tr -d '[:space:]'
}

# Returns the log-end offset for topic:partition from the live Kafka broker.
kafka_end_offset() {
  local topic="$1"
  ${DC} exec -T kafka kafka-get-offsets.sh \
    --bootstrap-server localhost:9092 \
    --topic-partitions "${topic}:0" \
    --time latest 2>/dev/null \
    | awk -F: '{print $NF}' | tr -d '[:space:]'
}

# ── 1. Collect image SHAs via docker inspect ──────────────────────────────────

log "Collecting image SHAs from running containers (docker inspect)..."

PIPELINE_SERVICES=(
  control-api
  ingest-clone
  expand-worker
  parse-worker
  typecheck-worker
  ingest-graph
  embed-worker
  projector-pg
  projector-neo4j
)

declare -A IMAGE_SHAS
sha_ok=0
for svc in "${PIPELINE_SERVICES[@]}"; do
  container=$(${DC} ps -q "${svc}" 2>/dev/null | head -1 || true)
  if [ -z "${container}" ]; then
    warn "No running container for service '${svc}'"
    fail_field "image_shas.${svc} — container not found (stack not running?)"
    IMAGE_SHAS["${svc}"]="MISSING"
    continue
  fi
  sha=$(docker inspect "${container}" --format='{{.Image}}' 2>/dev/null || true)
  if [ -z "${sha}" ] || [[ "${sha}" == "ERROR" ]]; then
    warn "docker inspect failed for ${svc} (container=${container})"
    fail_field "image_shas.${svc} — inspect returned empty"
    IMAGE_SHAS["${svc}"]="ERROR"
  else
    IMAGE_SHAS["${svc}"]="${sha}"
    sha_ok=$(( sha_ok + 1 ))
    log "  ${svc}: ${sha}"
  fi
done

if (( sha_ok == 0 )); then
  fail_field "image_shas — no containers found; is the compose stack running?"
fi

# Build image_shas JSON
IMAGE_SHAS_JSON=$(python3 -c "
import json, sys
shas = {}
$(for svc in "${PIPELINE_SERVICES[@]}"; do echo "shas['${svc}'] = '${IMAGE_SHAS[${svc}]}'"; done)
print(json.dumps(shas))
")

# ── 2. Collect DB row counts from live Postgres + Neo4j ───────────────────────

log "Collecting DB row counts from live databases..."

# Resolve tenant schema
if [ -z "${FINGERPRINT_TENANT_SCHEMA:-}" ]; then
  FINGERPRINT_TENANT_SCHEMA=$(psql_q \
    "SELECT schema_name FROM control.tenants ORDER BY created_at DESC LIMIT 1;" \
    | tr -d '[:space:]' || true)
fi

if [ -z "${FINGERPRINT_TENANT_SCHEMA:-}" ]; then
  fail_field "db — could not resolve tenant schema; no tenants in DB (no real signup ran)"
  ITEMS_COUNT="0"
else
  ITEMS_COUNT=$(psql_q \
    "SELECT COUNT(*) FROM \"${FINGERPRINT_TENANT_SCHEMA}\".code_symbols;" \
    | tr -d '[:space:]' || echo "0")
  if ! [[ "${ITEMS_COUNT}" =~ ^[0-9]+$ ]]; then
    fail_field "db.items — non-numeric result '${ITEMS_COUNT}'"
    ITEMS_COUNT="0"
  elif (( ITEMS_COUNT == 0 )); then
    fail_field "db.items — code_symbols count is 0 (no real ingest completed)"
  fi
  log "  items (code_symbols): ${ITEMS_COUNT}"
fi

# CALLS relationships from live Neo4j
CALLS_COUNT=$(neo4j_q "MATCH ()-[:CALLS]->() RETURN count(*) AS cnt;" || echo "0")
if ! [[ "${CALLS_COUNT}" =~ ^[0-9]+$ ]]; then
  fail_field "db.calls — non-numeric result '${CALLS_COUNT}'"
  CALLS_COUNT="0"
fi
log "  calls (Neo4j CALLS edges): ${CALLS_COUNT}"

# Total graph relations (all edge types)
RELATIONS_COUNT=$(neo4j_q "MATCH ()-[r]->() RETURN count(r) AS cnt;" || echo "0")
if ! [[ "${RELATIONS_COUNT}" =~ ^[0-9]+$ ]]; then
  fail_field "db.relations — non-numeric result '${RELATIONS_COUNT}'"
  RELATIONS_COUNT="0"
fi
log "  relations (Neo4j all edges): ${RELATIONS_COUNT}"

DB_JSON="{\"items\":${ITEMS_COUNT},\"calls\":${CALLS_COUNT},\"relations\":${RELATIONS_COUNT}}"

# ── 3. Collect Kafka topic log-end offsets ────────────────────────────────────

log "Collecting Kafka topic log-end offsets from live broker..."

KAFKA_TOPICS=(
  "rb.ingest.clone.commands"
  "rb.ingest.expand.commands"
  "rb.ingest.parse.commands"
  "rb.parsed-items.v1"
  "rb.ingest.typecheck.commands"
  "rb.typechecked-items.v1"
  "rb.ingest.graph.commands"
  "rb.ingest.embed.commands"
  "rb.source-files.v1"
  "rb.projector.events"
  "rb.audit.events"
)

declare -A KAFKA_OFFSETS
all_zero=1
for topic in "${KAFKA_TOPICS[@]}"; do
  offset=$(kafka_end_offset "${topic}" || echo "0")
  if ! [[ "${offset}" =~ ^[0-9]+$ ]]; then
    warn "Non-numeric offset for ${topic}: '${offset}'"
    offset="0"
  fi
  KAFKA_OFFSETS["${topic}"]="${offset}"
  log "  ${topic}: ${offset}"
  if (( offset > 0 )); then
    all_zero=0
  fi
done

if (( all_zero )); then
  fail_field "sse_offsets — all Kafka topic offsets are 0; no messages produced (mocked run has no live broker)"
fi

SSE_OFFSETS_JSON=$(python3 -c "
import json
offsets = {}
$(for topic in "${KAFKA_TOPICS[@]}"; do echo "offsets['${topic}'] = ${KAFKA_OFFSETS[${topic}]}"; done)
print(json.dumps(offsets))
")

# ── 4. Collect trace IDs from live DB ────────────────────────────────────────

log "Collecting trace IDs from control.ingestion_runs..."

if [ -n "${FINGERPRINT_INGEST_RUN_ID:-}" ]; then
  TRACE_QUERY="SELECT trace_id FROM control.ingestion_runs WHERE id = '${FINGERPRINT_INGEST_RUN_ID}' AND trace_id IS NOT NULL;"
else
  TRACE_QUERY="SELECT trace_id FROM control.ingestion_runs WHERE trace_id IS NOT NULL ORDER BY created_at DESC LIMIT 10;"
fi

TRACE_IDS_RAW=$(psql_q "${TRACE_QUERY}" 2>/dev/null | grep -v '^$' || true)

if [ -z "${TRACE_IDS_RAW}" ]; then
  fail_field "trace_ids — no trace_ids in ingestion_runs; real traces require live OTEL propagation"
  TRACE_IDS_JSON="[]"
else
  TRACE_IDS_JSON=$(echo "${TRACE_IDS_RAW}" | python3 -c \
    "import sys, json; ids = [l.strip() for l in sys.stdin if l.strip()]; print(json.dumps(ids))")
  log "  trace_ids: ${TRACE_IDS_JSON}"
fi

# ── 5. Emit fingerprint JSON ──────────────────────────────────────────────────

VERDICT="pass"
[ "${VERDICT_FAILED}" -ne 0 ] && VERDICT="fail"

log "Writing fingerprint to ${OUTPUT_FILE} (verdict=${VERDICT})..."

python3 - <<PYEOF
import json

fingerprint = {
    "schema_version": "1",
    "collected_at": "${TIMESTAMP}",
    "verdict": "${VERDICT}",
    "image_shas": ${IMAGE_SHAS_JSON},
    "db": ${DB_JSON},
    "sse_offsets": ${SSE_OFFSETS_JSON},
    "trace_ids": ${TRACE_IDS_JSON},
}

with open("${OUTPUT_FILE}", "w") as f:
    json.dump(fingerprint, f, indent=2)

print(json.dumps(fingerprint, indent=2))
PYEOF

# ── 6. Final verdict ──────────────────────────────────────────────────────────

if [ "${VERDICT_FAILED}" -ne 0 ]; then
  log "FINGERPRINT INVALID (verdict=fail) — required fields missing."
  log "A mocked or stubbed stack cannot produce a valid fingerprint:"
  log "  - Containers must be running (docker inspect requires real containers)"
  log "  - DB counts must be non-zero (requires a completed real ingest)"
  log "  - Kafka offsets must be non-zero (requires real message production)"
  log "  - trace_ids must be present (requires live OTEL propagation)"
  exit 1
fi

log "FINGERPRINT VALID (verdict=pass) — written to ${OUTPUT_FILE}"
