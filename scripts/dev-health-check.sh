#!/usr/bin/env bash
# Checks the rustbrain dev stack on mars and posts a Paperclip alert issue on failure.
# Run every 5 minutes via rustbrain-dev-health.timer.
#
# Usage:
#   scripts/dev-health-check.sh
#
# Environment:
#   COMPOSE_ENV_FILE         Path to compose env-file to source for port overrides.
#                            Required on mars so CONTROL_API_HOST_PORT etc. resolve
#                            to tailscale-remapped ports (e.g. 18080/15173/13300).
#                            Example: "/opt/rustbrain/compose/tailscale.env"
#   PAPERCLIP_API_URL        Paperclip API base URL (required for alerting)
#   PAPERCLIP_API_KEY        Paperclip API key (required for alerting)
#   PAPERCLIP_COMPANY_ID     Paperclip company ID (required for alerting)
#   ALERT_COOLDOWN_SECS      Minimum seconds between repeated alerts (default: 1800)
#   RB_STATE_DIR             State directory for cooldown tracking
#                            (default: $HOME/.local/state/rustbrain)
#
# Exit codes:
#   0  All checks passed
#   1  One or more checks failed (alert posted or suppressed by cooldown)
#
# See docs/dev-stack-health-alerting.md for setup instructions.

set -euo pipefail

# -- Source compose env-file for host port overrides -------------------------

if [[ -n "${COMPOSE_ENV_FILE:-}" && -f "$COMPOSE_ENV_FILE" ]]; then
  set -a
  # shellcheck source=/dev/null
  source "$COMPOSE_ENV_FILE"
  set +a
fi

CONTROL_API_PORT="${CONTROL_API_HOST_PORT:-8080}"
FRONTEND_PORT="${FRONTEND_HOST_PORT:-15173}"
LOKI_PORT="${LOKI_HOST_PORT:-3100}"
ALERT_COOLDOWN_SECS="${ALERT_COOLDOWN_SECS:-1800}"
STATE_DIR="${RB_STATE_DIR:-"$HOME/.local/state/rustbrain"}"
STATE_FILE="$STATE_DIR/dev-health-alert.last"
COMPOSE_PROJECT="rustbrain-dev"

mkdir -p "$STATE_DIR"

FAILURES=()

ts() { date -u '+%Y-%m-%dT%H:%M:%SZ'; }

# -- Check: critical Docker containers are running ---------------------------

check_compose_services() {
  local critical_services=("postgres" "control-api" "frontend" "kafka")

  for svc in "${critical_services[@]}"; do
    if ! docker ps \
        --filter "label=com.docker.compose.project=${COMPOSE_PROJECT}" \
        --filter "label=com.docker.compose.service=${svc}" \
        --filter "status=running" \
        --format "{{.Names}}" 2>/dev/null | grep -q .; then
      FAILURES+=("container '${svc}' is not running (project: ${COMPOSE_PROJECT})")
    fi
  done
}

# -- Check: HTTP health endpoints --------------------------------------------

check_http() {
  local name="$1" url="$2"
  local code
  code=$(curl -sf -o /dev/null -w "%{http_code}" \
    --connect-timeout 5 --max-time 10 "$url" 2>/dev/null || echo "000")
  if [[ "$code" != "200" ]]; then
    FAILURES+=("${name} returned HTTP ${code} (expected 200) at ${url}")
  fi
}

# -- Run all checks ----------------------------------------------------------

check_compose_services
check_http "control-api /health" "http://localhost:${CONTROL_API_PORT}/health"
check_http "frontend"             "http://localhost:${FRONTEND_PORT}/"
check_http "loki /ready"          "http://localhost:${LOKI_PORT}/ready"

# -- All healthy -------------------------------------------------------------

if [[ ${#FAILURES[@]} -eq 0 ]]; then
  echo "[dev-health-check] $(ts): all healthy"
  # Clear stale cooldown state when stack recovers so next failure alerts immediately.
  rm -f "$STATE_FILE"
  exit 0
fi

# -- Failures detected — apply cooldown to suppress alert spam ---------------

NOW=$(date +%s)

if [[ -f "$STATE_FILE" ]]; then
  LAST_ALERT=$(cat "$STATE_FILE" 2>/dev/null || echo 0)
  ELAPSED=$(( NOW - LAST_ALERT ))
  if (( ELAPSED < ALERT_COOLDOWN_SECS )); then
    echo "[dev-health-check] $(ts): failures detected, cooldown active (${ELAPSED}s < ${ALERT_COOLDOWN_SECS}s) — skipping alert"
    for f in "${FAILURES[@]}"; do
      echo "  - $f"
    done
    exit 1
  fi
fi

echo "$NOW" > "$STATE_FILE"

echo "[dev-health-check] $(ts): ALERT — failures detected:"
for f in "${FAILURES[@]}"; do
  echo "  - $f"
done

# -- Post Paperclip alert issue ----------------------------------------------

if [[ -z "${PAPERCLIP_API_URL:-}" || -z "${PAPERCLIP_API_KEY:-}" || -z "${PAPERCLIP_COMPANY_ID:-}" ]]; then
  echo "[dev-health-check] PAPERCLIP_API_URL/KEY/COMPANY_ID not set — skipping Paperclip alert" >&2
  exit 1
fi

FAILURE_BULLETS=""
for f in "${FAILURES[@]}"; do
  FAILURE_BULLETS+="- ${f}\n"
done

PAYLOAD=$(jq -n \
  --arg title "alert: dev stack failure on mars — $(date -u '+%Y-%m-%d %H:%M UTC')" \
  --arg desc "## Dev stack health alert

**Host:** mars (100.87.157.74)
**Time:** $(date -u)

### Failures

${FAILURE_BULLETS}
### Remediation

\`\`\`bash
# Check container status
docker compose -p rustbrain-dev ps

# Restart the stack
docker compose --env-file /opt/rustbrain/compose/tailscale.env \\
  -f /opt/rustbrain/compose/dev.yml \\
  -f /opt/rustbrain/compose/tailscale.yml up -d

# Check logs for a failing service
docker compose -p rustbrain-dev logs --tail=100 control-api
\`\`\`" \
  '{"title": $title, "description": $desc, "priority": "high", "status": "todo"}')

HTTP_CODE=$(curl -s -o /tmp/rb-dev-health-alert.json -w "%{http_code}" \
  -X POST \
  -H "Authorization: Bearer ${PAPERCLIP_API_KEY}" \
  -H "Content-Type: application/json" \
  -d "$PAYLOAD" \
  "${PAPERCLIP_API_URL}/api/companies/${PAPERCLIP_COMPANY_ID}/issues" 2>/dev/null || echo "000")

if [[ "$HTTP_CODE" == "200" || "$HTTP_CODE" == "201" ]]; then
  ISSUE_ID=$(jq -r '.id // "unknown"' /tmp/rb-dev-health-alert.json 2>/dev/null || echo "unknown")
  echo "[dev-health-check] Paperclip alert posted (issue: ${ISSUE_ID})"
else
  echo "[dev-health-check] failed to post Paperclip alert (HTTP ${HTTP_CODE})" >&2
  cat /tmp/rb-dev-health-alert.json >&2 2>/dev/null || true
fi

exit 1
