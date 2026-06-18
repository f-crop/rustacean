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
#   CRASH_LOOP_WINDOW_SECS   Sliding window for crash-loop detection (default: 600 = 10 min)
#   CRASH_LOOP_THRESHOLD     Max restarts within window before alerting (default: 3)
#   STUCK_STATE_SECS         Seconds a container must be stuck in Created/Restarting
#                            before alerting (default: 300 = 5 min)
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
ALERT_COOLDOWN_SECS="${ALERT_COOLDOWN_SECS:-1800}"
STATE_DIR="${RB_STATE_DIR:-"$HOME/.local/state/rustbrain"}"
STATE_FILE="$STATE_DIR/dev-health-alert.last"
CRASH_LOOP_WINDOW_SECS="${CRASH_LOOP_WINDOW_SECS:-600}"
CRASH_LOOP_THRESHOLD="${CRASH_LOOP_THRESHOLD:-3}"
STUCK_STATE_SECS="${STUCK_STATE_SECS:-300}"
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

# -- Check: container stuck in Created or Restarting state; crash-loops ------
# Inspects every rustbrain-dev container (not just critical ones) for:
#   1. Status == "created" for > STUCK_STATE_SECS  (never started — bind/env failure)
#   2. Status == "restarting" for > STUCK_STATE_SECS  (Docker can't get it running)
#   3. RestartCount increased by > CRASH_LOOP_THRESHOLD in CRASH_LOOP_WINDOW_SECS

check_worker_health() {
  local restart_dir="$STATE_DIR/restart-counts"
  local stuck_dir="$STATE_DIR/stuck-since"
  mkdir -p "$restart_dir" "$stuck_dir"

  local now_secs
  now_secs=$(date +%s)

  local cname
  while IFS= read -r cname; do
    [[ -z "$cname" ]] && continue
    _check_one_container "$cname" "$now_secs" "$restart_dir" "$stuck_dir"
  done < <(docker ps -a \
    --filter "label=com.docker.compose.project=${COMPOSE_PROJECT}" \
    --format "{{.Names}}" 2>/dev/null)
}

_check_one_container() {
  local cname="$1" now_secs="$2" restart_dir="$3" stuck_dir="$4"
  # Replace slashes in container name for use as a filename
  local safe_name="${cname//\//_}"

  # Single docker inspect call: status, creation time, restart count
  # Docker timestamps are ISO 8601 with nanoseconds, e.g. "2026-06-18T10:38:00.123456789Z"
  # GNU date -d handles this format natively on Linux/mars.
  local status created_at restart_count
  IFS=$'\t' read -r status created_at restart_count < <(
    docker inspect \
      --format '{{.State.Status}}	{{.Created}}	{{.RestartCount}}' \
      "$cname" 2>/dev/null || printf 'unknown\t1970-01-01T00:00:00Z\t0'
  )

  [[ "$status" == "unknown" ]] && return

  # --- Stuck in Created: container was created but never started ---
  # Caused by: port bind failure, missing env-file, entrypoint not found, etc.
  if [[ "$status" == "created" ]]; then
    local created_secs
    created_secs=$(date -d "$created_at" +%s 2>/dev/null || echo "$now_secs")
    local age=$(( now_secs - created_secs ))
    if (( age > STUCK_STATE_SECS )); then
      FAILURES+=("container '${cname}' stuck in 'created' state for $(( age / 60 )) min — never started (possible port bind or missing env-file)")
    fi
  fi

  # --- Stuck in Restarting: Docker is cycling but can't reach running ---
  # Track when we first observed the restarting state; alert if it persists.
  local restarting_file="$stuck_dir/${safe_name}.restarting"
  if [[ "$status" == "restarting" ]]; then
    if [[ ! -f "$restarting_file" ]]; then
      # First observation: record timestamp
      echo "$now_secs" > "$restarting_file"
    else
      local first_seen
      first_seen=$(cat "$restarting_file" 2>/dev/null || echo "$now_secs")
      local stuck_for=$(( now_secs - first_seen ))
      if (( stuck_for > STUCK_STATE_SECS )); then
        FAILURES+=("container '${cname}' stuck in 'restarting' state for $(( stuck_for / 60 )) min (RestartCount=${restart_count})")
      fi
    fi
  else
    # Container left restarting state — clear the marker so a future episode starts fresh
    rm -f "$restarting_file"
  fi

  # --- Crash-loop: RestartCount increased by >THRESHOLD in sliding window ---
  # State file: lines of "<epoch_secs> <restart_count>" ordered oldest-first.
  # On each run: prune entries outside the window, check delta, append current reading.
  local rc_file="$restart_dir/${safe_name}"
  local oldest_count_in_window=""
  local new_entries=()

  if [[ -f "$rc_file" ]]; then
    local ts_entry count_entry entry_age
    while IFS=' ' read -r ts_entry count_entry; do
      [[ -z "$ts_entry" ]] && continue
      entry_age=$(( now_secs - ts_entry ))
      if (( entry_age <= CRASH_LOOP_WINDOW_SECS )); then
        new_entries+=("$ts_entry $count_entry")
        # First (oldest) in-window entry establishes the baseline count
        [[ -z "$oldest_count_in_window" ]] && oldest_count_in_window="$count_entry"
      fi
    done < "$rc_file"
  fi

  # Append current reading before checking so it is included in the next run's window
  new_entries+=("$now_secs $restart_count")

  if [[ -n "$oldest_count_in_window" ]]; then
    local delta=$(( restart_count - oldest_count_in_window ))
    if (( delta > CRASH_LOOP_THRESHOLD )); then
      FAILURES+=("container '${cname}' crash-looping: ${delta} restarts in last $(( CRASH_LOOP_WINDOW_SECS / 60 )) min (RestartCount=${restart_count})")
    fi
  fi

  # Persist state (cap at 50 entries to bound file growth)
  printf '%s\n' "${new_entries[@]}" | tail -50 > "$rc_file"
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

# -- Check: in-network HTTP endpoint via docker exec -------------------------
# Used for services that do not publish a host port (e.g. loki). Probes the
# container internally so we don't depend on host-port mappings.

check_container_http() {
  local svc="$1" path="$2"
  local cid
  cid=$(docker ps \
      --filter "label=com.docker.compose.project=${COMPOSE_PROJECT}" \
      --filter "label=com.docker.compose.service=${svc}" \
      --filter "status=running" \
      --format "{{.ID}}" 2>/dev/null | head -n1)
  if [[ -z "$cid" ]]; then
    # Already reported by check_compose_services if svc was on the critical list.
    FAILURES+=("${svc} container not running — cannot probe ${path}")
    return
  fi
  # `wget -qO- -S` prints headers to stderr; we only need the exit status.
  if ! docker exec "$cid" wget -q --spider --timeout=5 "http://localhost${path}" 2>/dev/null; then
    FAILURES+=("${svc} in-network probe failed at http://localhost${path}")
  fi
}

# -- Run all checks ----------------------------------------------------------

check_compose_services
check_worker_health
check_http "control-api /health" "http://localhost:${CONTROL_API_PORT}/health"
check_http "frontend"             "http://localhost:${FRONTEND_PORT}/"
check_container_http "loki"       ":3100/ready"

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
