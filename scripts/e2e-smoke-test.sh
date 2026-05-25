#!/usr/bin/env bash
# Pipeline E2E smoke test (RUSAA-645).
#
# Starts the rb-e2e compose stack, runs a full 9-stage ingest cycle against a
# real GitHub repository (fixture: jarnura/rb-smoke-fixture), and asserts:
#   1. ingestion_runs.status reaches 'succeeded' within SMOKE_TIMEOUT_SECS
#   2. code_symbols count > 0 in the tenant Postgres schema
#   3. Neo4j contains at least one node with the ingested repo_id property
#
# Required env vars (set via GitHub Actions secrets in the smoke workflow):
#   SMOKE_GH_APP_ID              — GitHub App numeric ID
#   GITHUB_APP_PRIVATE_KEY_PEM   — raw RSA PEM private key for ingest-clone
#   SMOKE_INSTALLATION_ID        — numeric GitHub installation ID for the test repo
#   SMOKE_GITHUB_REPO_ID         — numeric GitHub repo ID
#   SMOKE_GITHUB_REPO_FULL_NAME  — e.g. "jarnura/rb-smoke-fixture"
#
# Optional:
#   SMOKE_TIMEOUT_SECS    — max seconds to wait for pipeline (default: 600)
#   SMOKE_POLL_INTERVAL   — polling interval in seconds (default: 10)
#
# Run from the project root:
#   bash scripts/e2e-smoke-test.sh

set -euo pipefail

COMPOSE_FILE="compose/e2e.yml"
DC="docker compose -f ${COMPOSE_FILE}"
API="http://localhost:18080"
TIMEOUT_SECS="${SMOKE_TIMEOUT_SECS:-600}"
POLL_INTERVAL="${SMOKE_POLL_INTERVAL:-10}"
COOKIE_JAR="/tmp/smoke-cookies-$$.txt"
SMOKE_FAILED=0

SMOKE_EMAIL="smoke@e2e.test"
SMOKE_PASS="smoke-password-e2e-123"

# UAT fingerprint output path (RUSAA-696).
# Populated at end of run; Gate 3 hook (RUSAA-695) requires this file.
UAT_FINGERPRINT_FILE="${UAT_FINGERPRINT_FILE:-/tmp/uat-fingerprint.json}"

# ── Helpers ───────────────────────────────────────────────────────────────────

log()  { echo "[$(date -u +%H:%M:%S)] [smoke] $*"; }
fail() { SMOKE_FAILED=1; log "FAIL: $*" >&2; exit 1; }

gen_uuid() { python3 -c "import uuid; print(uuid.uuid4())"; }

LOG_DIR="/tmp/smoke-compose-logs"

dump_logs() {
  log "--- container logs (last 100 lines per service) ---"
  mkdir -p "${LOG_DIR}"
  for svc in control-api ingest-clone expand-worker parse-worker typecheck-worker \
              ingest-graph embed-worker projector-pg projector-neo4j; do
    echo "=== ${svc} ==="
    ${DC} logs --no-color --tail 100 "${svc}" 2>/dev/null \
      | tee "${LOG_DIR}/${svc}.log" || true
  done
}

cleanup() {
  # Dump logs on any non-zero exit, not just explicit fail() calls.
  # set -e can abort without setting SMOKE_FAILED (unbound variable, broken pipe, etc.).
  local exit_code=$?
  [ "$exit_code" -eq 0 ] && [ "$SMOKE_FAILED" = "0" ] || dump_logs
  rm -f "$COOKIE_JAR"
  log "Tearing down compose stack..."
  ${DC} down -v --remove-orphans 2>/dev/null || true
}
trap cleanup EXIT

# Run a SQL statement against the shared postgres container (-t -A → plain rows).
psql_q() {
  ${DC} exec -T postgres psql -U rustbrain -d rustbrain -t -A -c "$1"
}

# Run a Cypher statement and return the scalar result on the last output line.
# Redirect stderr to suppress advisory startup lines that would corrupt tail -1.
neo4j_q() {
  ${DC} exec -T neo4j cypher-shell \
    -u neo4j -p rustbrain123 --format plain "$1" 2>/dev/null \
    | tail -1 | tr -d '[:space:]'
}

# ── Validate required env vars ────────────────────────────────────────────────

log "Checking required env vars..."
for var in SMOKE_GH_APP_ID GITHUB_APP_PRIVATE_KEY_PEM \
           SMOKE_INSTALLATION_ID SMOKE_GITHUB_REPO_ID SMOKE_GITHUB_REPO_FULL_NAME; do
  [ -n "${!var}" ] || fail "required env var ${var} is not set"
done
log "All required env vars present."

# ── Build and start compose stack ────────────────────────────────────────────

log "Building compose images..."
${DC} build

log "Starting compose stack (detached)..."
${DC} up -d

# ── Wait for control-api /health ──────────────────────────────────────────────

log "Waiting for control-api /health (up to 120 s)..."
deadline=$(( $(date +%s) + 120 ))
until curl -sf "${API}/health" >/dev/null 2>&1; do
  (( $(date +%s) < deadline )) || fail "control-api /health did not succeed within 120 s"
  sleep 3
done
log "control-api is healthy."

# ── Sign up a test user ───────────────────────────────────────────────────────

log "Signing up test user (${SMOKE_EMAIL})..."
curl -sf -X POST "${API}/v1/auth/signup" \
  -H "Content-Type: application/json" \
  -d "{\"email\":\"${SMOKE_EMAIL}\",\"password\":\"${SMOKE_PASS}\",\"tenant_name\":\"Smoke Tenant\"}" \
  -c "${COOKIE_JAR}" \
  >/dev/null
log "Signup complete."

# ── Verify email directly in Postgres (bypasses email transport) ──────────────

log "Marking email as verified in DB..."
psql_q "UPDATE control.users SET email_verified_at = now() WHERE email = '${SMOKE_EMAIL}';"

user_id=$(psql_q "SELECT id FROM control.users WHERE email = '${SMOKE_EMAIL}';" | tr -d '[:space:]')
[ -n "${user_id}" ] || fail "could not fetch user_id for ${SMOKE_EMAIL}"
log "user_id=${user_id}"

tenant_id=$(psql_q \
  "SELECT tenant_id FROM control.tenant_members WHERE user_id = '${user_id}' LIMIT 1;" \
  | tr -d '[:space:]')
[ -n "${tenant_id}" ] || fail "could not fetch tenant_id for user ${user_id}"
log "tenant_id=${tenant_id}"

# ── Re-login to get a fresh session that reflects verified email ──────────────

log "Logging in with verified session..."
rm -f "${COOKIE_JAR}"
curl -sf -X POST "${API}/v1/auth/login" \
  -H "Content-Type: application/json" \
  -d "{\"email\":\"${SMOKE_EMAIL}\",\"password\":\"${SMOKE_PASS}\"}" \
  -c "${COOKIE_JAR}" \
  >/dev/null
log "Logged in successfully."

# ── Seed GitHub installation + repo (bypass OAuth callback flow) ──────────────

INSTALL_UUID=$(gen_uuid)
REPO_UUID=$(gen_uuid)

log "Inserting github_installation (id=${INSTALL_UUID}, gh_id=${SMOKE_INSTALLATION_ID})..."
psql_q "INSERT INTO control.github_installations
  (id, tenant_id, github_installation_id, account_login, account_type, account_id)
  VALUES (
    '${INSTALL_UUID}',
    '${tenant_id}',
    ${SMOKE_INSTALLATION_ID},
    'smoke-org',
    'Organization',
    1
  );"

log "Inserting repo (id=${REPO_UUID}, full_name=${SMOKE_GITHUB_REPO_FULL_NAME})..."
psql_q "INSERT INTO control.repos
  (id, tenant_id, installation_id, github_repo_id, full_name, default_branch, connected_by)
  VALUES (
    '${REPO_UUID}',
    '${tenant_id}',
    '${INSTALL_UUID}',
    ${SMOKE_GITHUB_REPO_ID},
    '${SMOKE_GITHUB_REPO_FULL_NAME}',
    'main',
    '${user_id}'
  );"
log "Test fixture seeded."

# ── Trigger ingestion via API ─────────────────────────────────────────────────

log "Triggering ingestion for repo_id=${REPO_UUID}..."
trigger_resp=$(curl -sf -X POST "${API}/v1/repos/${REPO_UUID}/ingestions" \
  -H "Content-Type: application/json" \
  -d '{}' \
  -b "${COOKIE_JAR}")

ingest_run_id=$(echo "${trigger_resp}" \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['ingest_run_id'])" 2>/dev/null \
  || true)
[ -n "${ingest_run_id}" ] \
  || fail "trigger_ingestion did not return ingest_run_id. response=${trigger_resp}"
log "Ingestion queued: ingest_run_id=${ingest_run_id}"

# ── Poll ingestion_runs until 'succeeded' (or timeout / terminal failure) ─────

log "Polling for pipeline completion (timeout=${TIMEOUT_SECS} s, interval=${POLL_INTERVAL} s)..."
deadline=$(( $(date +%s) + TIMEOUT_SECS ))
run_status=""
while true; do
  run_status=$(psql_q \
    "SELECT status FROM control.ingestion_runs WHERE id = '${ingest_run_id}';")
  remaining=$(( deadline - $(date +%s) ))
  log "  status=${run_status} (${remaining} s remaining)"

  case "${run_status}" in
    succeeded)
      break
      ;;
    failed|cancelled)
      log "Stage breakdown:"
      psql_q "SELECT stage, status, error
              FROM control.pipeline_stage_runs
              WHERE ingestion_run_id = '${ingest_run_id}'
              ORDER BY stage;" || true
      fail "ingestion run ended with status=${run_status}"
      ;;
  esac

  if (( $(date +%s) >= deadline )); then
    log "Stage breakdown at timeout:"
    psql_q "SELECT stage, status
            FROM control.pipeline_stage_runs
            WHERE ingestion_run_id = '${ingest_run_id}'
            ORDER BY stage;" || true
    fail "pipeline did not complete within ${TIMEOUT_SECS} s (last status=${run_status})"
  fi

  sleep "${POLL_INTERVAL}"
done
log "Pipeline status=succeeded."

# ── Assert: code_symbols present in tenant Postgres schema ───────────────────

log "Asserting code_symbols in Postgres..."
tenant_schema=$(psql_q \
  "SELECT schema_name FROM control.tenants WHERE id = '${tenant_id}';" | tr -d '[:space:]')
[ -n "${tenant_schema}" ] \
  || fail "could not resolve schema_name for tenant ${tenant_id}"
log "  tenant_schema=${tenant_schema}"

symbol_count=$(psql_q \
  "SELECT COUNT(*) FROM \"${tenant_schema}\".code_symbols WHERE repo_id = '${REPO_UUID}';" \
  | tr -d '[:space:]')
log "  code_symbols count=${symbol_count}"
[[ "${symbol_count}" =~ ^[0-9]+$ ]] \
  || fail "non-numeric symbol_count from Postgres: '${symbol_count}' — query may have failed"
(( symbol_count > 0 )) \
  || fail "expected code_symbols > 0 in ${tenant_schema}, got ${symbol_count}"
log "Postgres assertion PASSED (${symbol_count} symbols)."

# ── Assert: nodes present in Neo4j ───────────────────────────────────────────

log "Asserting nodes in Neo4j..."
neo4j_count=$(neo4j_q \
  "MATCH (n {repo_id: '${REPO_UUID}'}) RETURN count(n) AS cnt;")
log "  neo4j node count=${neo4j_count}"
[[ "${neo4j_count}" =~ ^[0-9]+$ ]] \
  || fail "non-numeric neo4j_count from cypher-shell: '${neo4j_count}' — query may have failed"
(( neo4j_count > 0 )) \
  || fail "expected Neo4j nodes > 0 for repo_id=${REPO_UUID}, got ${neo4j_count}"
log "Neo4j assertion PASSED (${neo4j_count} nodes)."

# ── API seam assertions (RUSAA-676) ───────────────────────────────────────────
# These run after pipeline succeeds and assert the API layer returns correct
# data for the ingested repo — catching data-flow bugs that the DB assertions
# above cannot see (e.g. "source shows 1 line", "graph relationships empty").

# Helper: URL-safe base64 encode without padding (RFC 4648 §5).
b64url() { printf '%s' "$1" | base64 -w0 | tr '+/' '-_' | tr -d '='; }

# ── Discover a function FQN from Postgres ────────────────────────────────────
log "Discovering function FQN from code_symbols..."
ITEM_FQN=$(psql_q \
  "SELECT fqn FROM \"${tenant_schema}\".code_symbols
   WHERE repo_id = '${REPO_UUID}' AND kind IN ('Function','Method')
   AND source_path IS NOT NULL LIMIT 1;" | tr -d '[:space:]')
[ -n "${ITEM_FQN}" ] \
  || fail "could not find any Function/Method FQN in code_symbols for repo ${REPO_UUID}"
ITEM_FQN_B64=$(b64url "${ITEM_FQN}")
log "  item_fqn=${ITEM_FQN}"

# ── Assertion A: source preview has multiple lines ────────────────────────────
log "Asserting source preview (GET /v1/repos/.../items/...)..."
preview_resp=$(curl -sf -b "${COOKIE_JAR}" \
  "${API}/v1/repos/${REPO_UUID}/items/${ITEM_FQN_B64}")
preview_lines=$(echo "${preview_resp}" \
  | python3 -c "import sys,json; d=json.load(sys.stdin); print(len((d.get('source_preview') or '').split('\n')))")
log "  source_preview line count=${preview_lines}"
(( preview_lines > 1 )) \
  || fail "expected source_preview > 1 line for ${ITEM_FQN}, got ${preview_lines}"
log "Source preview assertion PASSED (${preview_lines} lines)."

# ── Assertion B: search returns results ──────────────────────────────────────
log "Asserting search (POST /v1/search)..."
search_resp=$(curl -sf -b "${COOKIE_JAR}" -X POST "${API}/v1/search" \
  -H "Content-Type: application/json" \
  -d "{\"q\":\"fn\",\"filters\":{\"repo_id\":\"${REPO_UUID}\"}}")
result_count=$(echo "${search_resp}" \
  | python3 -c "import sys,json; print(len(json.load(sys.stdin).get('results', [])))")
log "  search result count=${result_count}"
(( result_count > 0 )) \
  || fail "expected search results > 0 for repo ${REPO_UUID}, got ${result_count}"
log "Search assertion PASSED (${result_count} results)."

# ── Discover a callee FQN (function with incoming CALLS edges) ────────────────
log "Discovering callee FQN from Neo4j for callers assertion..."
CALLEE_FQN=$(neo4j_q \
  "MATCH ()-[:CALLS]->(n {repo_id: '${REPO_UUID}'}) RETURN n.fqn LIMIT 1;")
[ -n "${CALLEE_FQN}" ] \
  || fail "no CALLS relationships in Neo4j for repo_id=${REPO_UUID} — fixture may lack call relationships"
CALLEE_FQN_B64=$(b64url "${CALLEE_FQN}")
log "  callee_fqn=${CALLEE_FQN}"

# ── Assertion C: callers graph has nodes ──────────────────────────────────────
log "Asserting callers graph (GET /v1/repos/.../items/.../callers)..."
callers_resp=$(curl -sf -b "${COOKIE_JAR}" \
  "${API}/v1/repos/${REPO_UUID}/items/${CALLEE_FQN_B64}/callers")
callers_count=$(echo "${callers_resp}" \
  | python3 -c "import sys,json; print(len(json.load(sys.stdin).get('nodes', [])))")
log "  callers node count=${callers_count}"
(( callers_count > 0 )) \
  || fail "expected callers nodes > 0 for ${CALLEE_FQN}, got ${callers_count}"
log "Callers graph assertion PASSED (${callers_count} callers)."

# ── Assertion D: module tree has children ─────────────────────────────────────
log "Asserting module tree (GET /v1/repos/.../modules)..."
modules_resp=$(curl -sf -b "${COOKIE_JAR}" "${API}/v1/repos/${REPO_UUID}/modules")
children_count=$(echo "${modules_resp}" \
  | python3 -c "import sys,json; print(len(json.load(sys.stdin).get('tree',{}).get('children', [])))")
log "  module tree children count=${children_count}"
(( children_count > 0 )) \
  || fail "expected module tree children > 0, got ${children_count}"
log "Module tree assertion PASSED (${children_count} modules)."

# ── Assertion E: health endpoint reports all stores OK ───────────────────────
log "Asserting health endpoint (GET /health)..."
health_resp=$(curl -sf "${API}/health")
neo4j_status=$(echo "${health_resp}" \
  | python3 -c "import sys,json; print(json.load(sys.stdin).get('stores',{}).get('neo4j',''))")
qdrant_status=$(echo "${health_resp}" \
  | python3 -c "import sys,json; print(json.load(sys.stdin).get('stores',{}).get('qdrant',''))")
log "  stores.neo4j=${neo4j_status}  stores.qdrant=${qdrant_status}"
[ "${neo4j_status}" = "ok" ] \
  || fail "expected stores.neo4j=ok, got '${neo4j_status}'"
[ "${qdrant_status}" = "ok" ] \
  || fail "expected stores.qdrant=ok, got '${qdrant_status}'"
log "Health assertion PASSED (neo4j=${neo4j_status}, qdrant=${qdrant_status})."

# ── Board happy-path assertions (RUSAA-1669) ──────────────────────────────────
# These run after the full pipeline succeeds and validate the board-level UX
# flows: MCP tools, agent session creation, and NDJSON log streaming.
# The pipeline steps above already prove signup → install → repo → ingestion.

E2E_HEADERS_FILE="/tmp/smoke-mcp-headers-$$.txt"
trap 'rm -f "$E2E_HEADERS_FILE"' RETURN 2>/dev/null || true

# ── Board step B1: MCP initialize ────────────────────────────────────────────
log "Board B1: MCP initialize (POST ${API}/mcp)..."
mcp_init_body=$(curl -sf -X POST "${API}/mcp" \
  -H "Content-Type: application/json" \
  -D "${E2E_HEADERS_FILE}" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","clientInfo":{"name":"e2e-smoke","version":"1.0.0"}}}' \
  -b "${COOKIE_JAR}") || fail "MCP initialize request failed"

mcp_session_id=$(grep -i "^mcp-session-id:" "${E2E_HEADERS_FILE}" \
  | awk '{print $2}' | tr -d '[:space:]\r')
[ -n "${mcp_session_id}" ] \
  || fail "MCP initialize did not return Mcp-Session-Id header (body: ${mcp_init_body})"
log "  mcp_session_id=${mcp_session_id}"

# ── Board step B2: MCP tools/list ────────────────────────────────────────────
log "Board B2: MCP tools/list (POST ${API}/mcp)..."
tools_resp=$(curl -sf -X POST "${API}/mcp" \
  -H "Content-Type: application/json" \
  -H "Mcp-Session-Id: ${mcp_session_id}" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  -b "${COOKIE_JAR}") || fail "MCP tools/list request failed"

tool_count=$(echo "${tools_resp}" \
  | python3 -c "import sys,json; d=json.load(sys.stdin); print(len(d.get('result',{}).get('tools',[])))" 2>/dev/null || true)
log "  tool count=${tool_count}"
[[ "${tool_count}" =~ ^[0-9]+$ ]] \
  || fail "non-numeric tool_count from MCP tools/list: '${tool_count}'"
(( tool_count >= 2 )) \
  || fail "expected ≥ 2 MCP tools, got ${tool_count}"
log "MCP tools/list PASSED (${tool_count} tools)."

# ── Board step B3: Agent session creation ────────────────────────────────────
log "Board B3: Creating agent session (POST ${API}/v1/agents/sessions)..."
session_resp=$(curl -sf -X POST "${API}/v1/agents/sessions" \
  -H "Content-Type: application/json" \
  -d '{"runtime":"claude_code","initial_prompt":"e2e smoke board happy-path"}' \
  -b "${COOKIE_JAR}" \
  -w "\n%{http_code}") || fail "agent session create request failed"

session_http_status=$(echo "${session_resp}" | tail -1)
session_body=$(echo "${session_resp}" | head -n -1)
[ "${session_http_status}" -eq 202 ] \
  || fail "create session returned HTTP ${session_http_status}, expected 202 (body: ${session_body})"

session_id=$(echo "${session_body}" \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['session_id'])" 2>/dev/null || true)
[ -n "${session_id}" ] \
  || fail "create session response missing session_id (body: ${session_body})"

db_session_status=$(psql_q \
  "SELECT status FROM agents.agent_sessions WHERE id = '${session_id}';" | tr -d '[:space:]')
[ "${db_session_status}" = "pending" ] \
  || fail "expected agents.agent_sessions.status=pending, got '${db_session_status}'"
log "Agent session PASSED: session_id=${session_id} status=${db_session_status}."

# ── Board step B4: Seed events + NDJSON streaming ────────────────────────────
log "Board B4: Seeding agent events for NDJSON streaming test..."
psql_q "INSERT INTO agents.agent_events
        (id, session_id, tenant_id, event_type, sequence, payload, created_at)
        VALUES
          (gen_random_uuid(), '${session_id}', '${tenant_id}', 'session.created', 1, '{\"status\":\"created\"}'::jsonb, now()),
          (gen_random_uuid(), '${session_id}', '${tenant_id}', 'session.message', 2, '{\"text\":\"e2e smoke hello\"}'::jsonb, now());"

log "Board B4: NDJSON log streaming (GET ${API}/v1/agents/sessions/${session_id}/log.ndjson)..."
ndjson_resp=$(curl -sf -b "${COOKIE_JAR}" \
  "${API}/v1/agents/sessions/${session_id}/log.ndjson") || fail "NDJSON log request failed"

ndjson_lines=$(echo "${ndjson_resp}" | grep -c '^{' || true)
log "  NDJSON line count=${ndjson_lines}"
(( ndjson_lines >= 2 )) \
  || fail "expected ≥ 2 NDJSON lines, got ${ndjson_lines}"
log "NDJSON streaming PASSED (${ndjson_lines} lines)."

rm -f "${E2E_HEADERS_FILE}"

# ── Collect UAT fingerprint (RUSAA-696 — Pillar C) ───────────────────────────
# Must run before the stack tears down (cleanup trap fires on EXIT).
# Scopes fingerprint to this ingest run + the tenant resolved above.

log "Collecting UAT fingerprint (RUSAA-696)..."
FINGERPRINT_INGEST_RUN_ID="${ingest_run_id}" \
FINGERPRINT_TENANT_SCHEMA="${tenant_schema}" \
  bash "$(dirname "$0")/uat-fingerprint.sh" \
    "${COMPOSE_FILE}" "${UAT_FINGERPRINT_FILE}" \
  || { log "WARN: fingerprint collection failed — Gate 3 will reject this verdict"; }

log "Fingerprint written to ${UAT_FINGERPRINT_FILE}"

# ── Done ──────────────────────────────────────────────────────────────────────

log "E2E pipeline smoke test PASSED."
