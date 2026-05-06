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

# ── API-level seam assertions (RUSAA-676) ─────────────────────────────────────
#
# Five checks that verify data flows all the way through to the HTTP layer.
# These catch bugs that slip past DB-layer checks (e.g. source_preview=1 line,
# empty callers graph) because they exercise the full service call path.

log "=== API-level seam assertions (RUSAA-676) ==="

# Seam 1: /health reports neo4j and qdrant as "ok".
# Validates that all stores are reachable after the full pipeline run —
# a degraded store means subsequent seam assertions would give false results.
log "[seam 1/5] health endpoint: neo4j and qdrant must report ok..."
health_resp=$(curl -sf "${API}/health")
neo4j_health=$(echo "${health_resp}" \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['stores']['neo4j'])" \
  2>/dev/null || echo "parse_error")
qdrant_health=$(echo "${health_resp}" \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['stores']['qdrant'])" \
  2>/dev/null || echo "parse_error")
[ "${neo4j_health}" = "ok" ] \
  || fail "[seam 1/5] health: neo4j=${neo4j_health}, expected ok"
[ "${qdrant_health}" = "ok" ] \
  || fail "[seam 1/5] health: qdrant=${qdrant_health}, expected ok"
log "[seam 1/5] PASSED (neo4j=${neo4j_health}, qdrant=${qdrant_health})"

# Seam 2: GET /v1/repos/{repo_id}/items/{fqn_b64} returns source_preview with > 1 line.
# Catches the "source shows 1 line" class of bug where source_text was stored
# truncated or the typecheck-worker wrote only the signature line.
log "[seam 2/5] source_preview: multi-line function must return > 1 preview line..."
preview_fqn=$(psql_q \
  "SELECT fqn FROM \"${tenant_schema}\".code_symbols
   WHERE repo_id = '${REPO_UUID}' AND kind = 'FN'
     AND line_start IS NOT NULL AND line_end IS NOT NULL
     AND line_end - line_start > 1
   LIMIT 1;" | tr -d '[:space:]')
[ -n "${preview_fqn}" ] \
  || fail "[seam 2/5] no multi-line function (kind=FN, line_end-line_start>1) in code_symbols — fixture may need updating"
fqn_b64=$(printf '%s' "${preview_fqn}" | base64 -w 0 | tr '+/' '-_' | tr -d '=')
item_resp=$(curl -sf "${API}/v1/repos/${REPO_UUID}/items/${fqn_b64}" -b "${COOKIE_JAR}")
preview_lines=$(echo "${item_resp}" \
  | python3 -c "import sys,json; print(len((json.load(sys.stdin).get('source_preview') or '').splitlines()))" \
  2>/dev/null || echo "0")
(( preview_lines > 1 )) \
  || fail "[seam 2/5] source_preview lines=${preview_lines} for fqn=${preview_fqn}, expected > 1"
log "[seam 2/5] PASSED (fqn=${preview_fqn}, lines=${preview_lines})"

# Seam 3: POST /v1/search returns at least one result scoped to this repo.
# Validates the embed-worker → Qdrant → search path end-to-end.
log "[seam 3/5] search: must return > 0 results for the ingested repo..."
search_resp=$(curl -sf -X POST "${API}/v1/search" \
  -H "Content-Type: application/json" \
  -d "{\"q\":\"fn\",\"filters\":{\"repo_id\":\"${REPO_UUID}\"}}" \
  -b "${COOKIE_JAR}")
search_count=$(echo "${search_resp}" \
  | python3 -c "import sys,json; print(len(json.load(sys.stdin)['results']))" \
  2>/dev/null || echo "0")
(( search_count > 0 )) \
  || fail "[seam 3/5] search returned ${search_count} results for repo_id=${REPO_UUID}, expected > 0"
log "[seam 3/5] PASSED (results=${search_count})"

# Seam 4: GET /v1/repos/{repo_id}/items/{fqn_b64}/callers returns nodes.
# Validates the ingest-graph CALLS extraction → Neo4j → traversal API path.
# First pick a callee FQN that has at least one CALLS edge in Neo4j, then hit the API.
log "[seam 4/5] callers: a called function must have callers visible via the API..."
callee_fqn=$(${DC} exec -T neo4j cypher-shell \
  -u neo4j -p rustbrain123 --format plain \
  "MATCH ()-[:CALLS]->(b {repo_id: '${REPO_UUID}'}) RETURN b.fqn LIMIT 1;" \
  2>/dev/null | tail -1 | tr -d '[:space:]"')
[ -n "${callee_fqn}" ] \
  || fail "[seam 4/5] no CALLS edges found in Neo4j for repo_id=${REPO_UUID} — CALLS extraction may have failed"
callee_b64=$(printf '%s' "${callee_fqn}" | base64 -w 0 | tr '+/' '-_' | tr -d '=')
callers_resp=$(curl -sf "${API}/v1/repos/${REPO_UUID}/items/${callee_b64}/callers" \
  -b "${COOKIE_JAR}")
caller_nodes=$(echo "${callers_resp}" \
  | python3 -c "import sys,json; print(len(json.load(sys.stdin)['nodes']))" \
  2>/dev/null || echo "0")
(( caller_nodes > 0 )) \
  || fail "[seam 4/5] callers endpoint returned ${caller_nodes} nodes for fqn=${callee_fqn}, expected > 0"
log "[seam 4/5] PASSED (callee=${callee_fqn}, caller_nodes=${caller_nodes})"

# Seam 5: GET /v1/repos/{repo_id}/modules returns a tree with children.
# Validates the module-tree builder reads code_symbols correctly and the fixture
# has at least 2 modules (root + at least one child).
log "[seam 5/5] module tree: must have at least one child module..."
modules_resp=$(curl -sf "${API}/v1/repos/${REPO_UUID}/modules" -b "${COOKIE_JAR}")
module_children=$(echo "${modules_resp}" \
  | python3 -c "import sys,json; print(len(json.load(sys.stdin)['tree']['children']))" \
  2>/dev/null || echo "0")
(( module_children > 0 )) \
  || fail "[seam 5/5] module tree has ${module_children} children for repo_id=${REPO_UUID}, expected > 0"
log "[seam 5/5] PASSED (module_children=${module_children})"

log "All 5 API-level seam assertions PASSED."

# ── Done ──────────────────────────────────────────────────────────────────────

log "E2E pipeline smoke test PASSED."
