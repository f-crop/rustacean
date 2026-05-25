#!/usr/bin/env bash
# Board happy-path smoke test (RUSAA-1669).
#
# Exercises the user-facing board flows end-to-end against the board-smoke
# compose stack. No GitHub App credentials are required.
#
# Steps validated:
#   1. Signup (email transport = console; verify directly in Postgres)
#   2. Login (session cookie)
#   3. MCP initialize → obtain Mcp-Session-Id
#   4. MCP tools/list → assert ≥ 2 tools registered (search_items, get_item)
#   5. Create agent session → assert pending row in DB
#   6. Seed agent events via SQL
#   7. NDJSON log streaming → assert ≥ 1 line returned
#
# Usage:
#   bash scripts/board-smoke.sh
#
# Optional env vars:
#   BOARD_SMOKE_TIMEOUT_SECS   — health-wait timeout (default: 120)
#   BOARD_SMOKE_POLL_INTERVAL  — polling interval   (default: 3)

set -euo pipefail

COMPOSE_FILE="compose/board-smoke.yml"
DC="docker compose -f ${COMPOSE_FILE}"
API="http://localhost:18081"
TIMEOUT_SECS="${BOARD_SMOKE_TIMEOUT_SECS:-120}"
POLL_INTERVAL="${BOARD_SMOKE_POLL_INTERVAL:-3}"
COOKIE_JAR="/tmp/board-smoke-cookies-$$.txt"
HEADERS_FILE="/tmp/board-smoke-headers-$$.txt"
SMOKE_FAILED=0

SMOKE_EMAIL="board-smoke@e2e.test"
SMOKE_PASS="board-smoke-pw-e2e-123"

# ── Helpers ───────────────────────────────────────────────────────────────────

log()  { echo "[$(date -u +%H:%M:%S)] [board-smoke] $*"; }
fail() { SMOKE_FAILED=1; log "FAIL: $*" >&2; exit 1; }

LOG_DIR="/tmp/board-smoke-compose-logs"

dump_logs() {
  log "--- container logs (last 100 lines per service) ---"
  mkdir -p "${LOG_DIR}"
  for svc in control-api kafka; do
    echo "=== ${svc} ==="
    ${DC} logs --no-color --tail 100 "${svc}" 2>/dev/null \
      | tee "${LOG_DIR}/${svc}.log" || true
  done
}

cleanup() {
  local exit_code=$?
  [ "$exit_code" -eq 0 ] && [ "$SMOKE_FAILED" = "0" ] || dump_logs
  rm -f "$COOKIE_JAR" "$HEADERS_FILE"
  log "Tearing down compose stack..."
  ${DC} down -v --remove-orphans 2>/dev/null || true
}
trap cleanup EXIT

# Run a SQL statement against the postgres container.
psql_q() {
  ${DC} exec -T postgres psql -U rustbrain -d rustbrain -t -A -c "$1"
}

# ── Build and start compose stack ─────────────────────────────────────────────

log "Building compose images (board-smoke)..."
${DC} build

log "Starting compose stack (detached)..."
${DC} up -d

# ── Wait for control-api /health ──────────────────────────────────────────────

log "Waiting for control-api /health (up to ${TIMEOUT_SECS} s)..."
deadline=$(( $(date +%s) + TIMEOUT_SECS ))
until curl -sf "${API}/health" >/dev/null 2>&1; do
  (( $(date +%s) < deadline )) || fail "control-api /health did not respond within ${TIMEOUT_SECS} s"
  sleep "${POLL_INTERVAL}"
done
log "control-api is healthy."

# ── Step 1: Signup ────────────────────────────────────────────────────────────

log "Step 1: Signing up test user (${SMOKE_EMAIL})..."
signup_resp=$(curl -sf -X POST "${API}/v1/auth/signup" \
  -H "Content-Type: application/json" \
  -d "{\"email\":\"${SMOKE_EMAIL}\",\"password\":\"${SMOKE_PASS}\",\"tenant_name\":\"Board Smoke Tenant\"}" \
  -c "${COOKIE_JAR}" \
  -w "\n%{http_code}" 2>&1) || fail "signup request failed"

signup_status=$(echo "${signup_resp}" | tail -1)
[ "${signup_status}" -eq 200 ] || [ "${signup_status}" -eq 201 ] \
  || fail "signup returned HTTP ${signup_status}, expected 200/201"
log "Signup complete (HTTP ${signup_status})."

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

# ── Step 2: Login ─────────────────────────────────────────────────────────────

log "Step 2: Logging in with verified session..."
rm -f "${COOKIE_JAR}"
login_status=$(curl -sf -X POST "${API}/v1/auth/login" \
  -H "Content-Type: application/json" \
  -d "{\"email\":\"${SMOKE_EMAIL}\",\"password\":\"${SMOKE_PASS}\"}" \
  -c "${COOKIE_JAR}" \
  -w "%{http_code}" \
  -o /dev/null) || fail "login request failed"

[ "${login_status}" -eq 200 ] \
  || fail "login returned HTTP ${login_status}, expected 200"
log "Login successful."

# ── Step 3: MCP initialize ────────────────────────────────────────────────────

log "Step 3: MCP initialize (POST /mcp)..."
mcp_init_body=$(curl -sf -X POST "${API}/mcp" \
  -H "Content-Type: application/json" \
  -D "${HEADERS_FILE}" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","clientInfo":{"name":"board-smoke","version":"1.0.0"}}}' \
  -b "${COOKIE_JAR}") || fail "MCP initialize request failed"

mcp_session_id=$(grep -i "^mcp-session-id:" "${HEADERS_FILE}" \
  | awk '{print $2}' | tr -d '[:space:]\r')
[ -n "${mcp_session_id}" ] \
  || fail "MCP initialize did not return Mcp-Session-Id header (body: ${mcp_init_body})"
log "MCP session created: mcp_session_id=${mcp_session_id}"

# Validate the initialize response has the expected shape.
mcp_init_result=$(echo "${mcp_init_body}" \
  | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('result',{}).get('protocolVersion',''))" 2>/dev/null || true)
[ -n "${mcp_init_result}" ] \
  || fail "MCP initialize response missing result.protocolVersion (body: ${mcp_init_body})"
log "MCP protocol version: ${mcp_init_result}"

# ── Step 4: MCP tools/list ────────────────────────────────────────────────────

log "Step 4: MCP tools/list (POST /mcp)..."
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
  || fail "expected ≥ 2 MCP tools (search_items + get_item), got ${tool_count}"

tool_names=$(echo "${tools_resp}" \
  | python3 -c "import sys,json; d=json.load(sys.stdin); print(' '.join(t['name'] for t in d.get('result',{}).get('tools',[])))")
log "MCP tools/list PASSED: ${tool_count} tools (${tool_names})."

# ── Step 5: Create agent session ──────────────────────────────────────────────

log "Step 5: Creating agent session (POST /v1/agents/sessions)..."
session_resp=$(curl -sf -X POST "${API}/v1/agents/sessions" \
  -H "Content-Type: application/json" \
  -d '{"runtime":"claude_code","initial_prompt":"board smoke test session"}' \
  -b "${COOKIE_JAR}" \
  -w "\n%{http_code}") || fail "agent session create request failed"

session_status=$(echo "${session_resp}" | tail -1)
session_body=$(echo "${session_resp}" | head -n -1)

[ "${session_status}" -eq 202 ] \
  || fail "create session returned HTTP ${session_status}, expected 202 (body: ${session_body})"

session_id=$(echo "${session_body}" \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['session_id'])" 2>/dev/null || true)
[ -n "${session_id}" ] \
  || fail "create session response missing session_id (body: ${session_body})"
log "  session_id=${session_id}"

# Assert the session row exists in DB with status=pending.
db_status=$(psql_q \
  "SELECT status FROM agents.agent_sessions WHERE id = '${session_id}';" | tr -d '[:space:]')
[ "${db_status}" = "pending" ] \
  || fail "expected agents.agent_sessions.status=pending, got '${db_status}'"
log "Agent session PASSED: session_id=${session_id} status=${db_status}."

# ── Step 6: Seed agent events ─────────────────────────────────────────────────

log "Step 6: Seeding agent events for session ${session_id}..."
psql_q "INSERT INTO agents.agent_events
        (id, session_id, tenant_id, event_type, sequence, payload, created_at)
        VALUES
          (gen_random_uuid(), '${session_id}', '${tenant_id}', 'session.created',  1, '{\"status\":\"created\"}'::jsonb, now()),
          (gen_random_uuid(), '${session_id}', '${tenant_id}', 'session.message',  2, '{\"text\":\"board smoke hello\"}'::jsonb, now());"
log "Seeded 2 agent events."

# ── Step 7: NDJSON log streaming ──────────────────────────────────────────────

log "Step 7: NDJSON log streaming (GET /v1/agents/sessions/${session_id}/log.ndjson)..."
ndjson_resp=$(curl -sf -b "${COOKIE_JAR}" \
  "${API}/v1/agents/sessions/${session_id}/log.ndjson") || fail "NDJSON log request failed"

ndjson_lines=$(echo "${ndjson_resp}" | grep -c '^{' || true)
log "  NDJSON line count=${ndjson_lines}"

(( ndjson_lines >= 2 )) \
  || fail "expected ≥ 2 NDJSON log lines (one per seeded event), got ${ndjson_lines}"

# Validate the first line is parseable JSON with expected fields.
first_line=$(echo "${ndjson_resp}" | head -1)
event_type=$(echo "${first_line}" \
  | python3 -c "import sys,json; print(json.loads(sys.stdin.read())['event_type'])" 2>/dev/null || true)
[ -n "${event_type}" ] \
  || fail "first NDJSON line is not parseable JSON or missing event_type: ${first_line}"
log "NDJSON streaming PASSED: ${ndjson_lines} lines, first event_type=${event_type}."

# ── Done ──────────────────────────────────────────────────────────────────────

log "Board happy-path smoke test PASSED."
log "  Steps covered:"
log "    1. Signup + email verification"
log "    2. Login"
log "    3. MCP initialize (Mcp-Session-Id obtained)"
log "    4. MCP tools/list (${tool_count} tools: ${tool_names})"
log "    5. Agent session created (status=pending)"
log "    6. Agent events seeded"
log "    7. NDJSON log streaming (${ndjson_lines} lines)"
