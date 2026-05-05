#!/usr/bin/env bash
# Rebuilds and restarts dev-stack services whose source paths changed between two git SHAs.
#
# Usage:
#   scripts/dev-stack-auto-rebuild.sh [--cold-start] [PREV_SHA [NEW_SHA]]
#   scripts/dev-stack-auto-rebuild.sh --logs [N]    # show last N rebuild records (default: 10)
#
# If SHAs are omitted, detects PREV_SHA=HEAD^1 and NEW_SHA=HEAD from the repo.
# --cold-start forces a full up -d of the entire stack regardless of which paths changed.
#
# Environment:
#   COMPOSE_CMD      Full docker compose invocation (default: "docker compose -f <repo>/compose/dev.yml")
#                    Mars example: "docker compose --env-file compose/tailscale.env -f compose/dev.yml -f compose/tailscale.yml"
#   COMPOSE_ENV_FILE Path to a docker compose env-file to source into the shell before health checks.
#                    Required on mars so CONTROL_API_HOST_PORT/FRONTEND_HOST_PORT resolve to the
#                    remapped ports (e.g. 18080/15173) rather than the dev defaults (8080/15173).
#                    Example: "/opt/rustbrain/compose/tailscale.env"
#   RB_REPO_PATH     Repo root path (default: parent of this script)
#   GITHUB_TOKEN     If set, posts a commit status to GitHub for NEW_SHA
#   GITHUB_REPO      Required with GITHUB_TOKEN (e.g. "jarnura/rustacean")
#   RB_LOG_DIR       Log directory (default: $HOME/.local/state/rustbrain)
#
# Bypass: touch compose/.no-auto-rebuild in the repo root to skip the next rebuild cycle.
# The file is removed after being honoured. See docs/dev-stack-auto-rebuild.md.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${RB_REPO_PATH:-"$(cd "$SCRIPT_DIR/.." && pwd)"}"
LOG_DIR="${RB_LOG_DIR:-"$HOME/.local/state/rustbrain"}"
LOG_FILE="$LOG_DIR/dev-stack-rebuilds.ndjson"
BYPASS_FILE="$REPO_ROOT/compose/.no-auto-rebuild"

# Source the compose env-file into the shell so host-port overrides
# (e.g. CONTROL_API_HOST_PORT=18080 on mars) are visible during health checks.
# docker compose's --env-file flag only passes vars to containers, not to this shell.
if [[ -n "${COMPOSE_ENV_FILE:-}" && -f "$COMPOSE_ENV_FILE" ]]; then
  set -a
  # shellcheck source=/dev/null
  source "$COMPOSE_ENV_FILE"
  set +a
fi

# -- Helpers -----------------------------------------------------------------

ts() { date -u +%Y-%m-%dT%H:%M:%SZ; }

log_record() {
  local result="$1" health="$2" rebuilt_json="$3" reason="$4"
  local elapsed=$(( $(date +%s) - ELAPSED_START ))
  python3 -c "
import json, sys
print(json.dumps({
  'timestamp':  sys.argv[1],
  'prev_sha':   sys.argv[2],
  'new_sha':    sys.argv[3],
  'result':     sys.argv[4],
  'health':     sys.argv[5],
  'rebuilt':    json.loads(sys.argv[6]),
  'reason':     sys.argv[7],
  'elapsed_s':  int(sys.argv[8]),
}))
" "$START_TS" "$PREV_SHA" "$NEW_SHA" "$result" "$health" "$rebuilt_json" "$reason" "$elapsed" >> "$LOG_FILE"
}

post_gh_status() {
  local state="$1" desc="$2"
  [[ -z "${GITHUB_TOKEN:-}" || -z "${GITHUB_REPO:-}" ]] && return 0
  local body
  body="$(python3 -c "
import json, sys
print(json.dumps({'state': sys.argv[1], 'description': sys.argv[2], 'context': 'dev-stack/auto-rebuild'}))
" "$state" "$desc")"
  curl -s -o /dev/null \
    -H "Authorization: token $GITHUB_TOKEN" \
    -H "Content-Type: application/json" \
    -d "$body" \
    "https://api.github.com/repos/${GITHUB_REPO}/statuses/${NEW_SHA}" || true
}

# -- Flag parsing ------------------------------------------------------------

COLD_START=false
if [[ "${1:-}" == "--cold-start" ]]; then
  COLD_START=true
  shift
fi

# -- Logs mode ---------------------------------------------------------------

if [[ "${1:-}" == "--logs" ]]; then
  N="${2:-10}"
  if [[ ! -f "$LOG_FILE" ]]; then
    echo "No rebuild log at $LOG_FILE"
    exit 0
  fi
  tail -n "$N" "$LOG_FILE" | python3 -c "
import sys, json
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        r = json.loads(line)
        sha = r.get('new_sha', '?')[:8]
        svc = ','.join(r.get('rebuilt', [])) or '-'
        print(f\"{r.get('timestamp','')}  sha={sha}  services={svc}  result={r.get('result','?')}  health={r.get('health','?')}  elapsed={r.get('elapsed_s','?')}s\")
    except Exception:
        print(line)
"
  exit 0
fi

# -- Main --------------------------------------------------------------------

PREV_SHA="${1:-}"
NEW_SHA="${2:-}"

cd "$REPO_ROOT"

if [[ -z "$PREV_SHA" || -z "$NEW_SHA" ]]; then
  NEW_SHA="$(git rev-parse HEAD)"
  PREV_SHA="$(git rev-parse HEAD^1 2>/dev/null || git rev-parse "$(git rev-list --max-parents=0 HEAD)")"
fi

START_TS="$(ts)"
ELAPSED_START="$(date +%s)"
mkdir -p "$LOG_DIR"

echo "[dev-stack-auto-rebuild] $START_TS  $PREV_SHA → $NEW_SHA"

# -- Bypass check ------------------------------------------------------------

if [[ -f "$BYPASS_FILE" ]]; then
  echo "[dev-stack-auto-rebuild] bypass file found — skipping this cycle"
  rm -f "$BYPASS_FILE"
  log_record "skipped" "" "[]" "bypass file"
  exit 0
fi

# -- Detect changed paths (skipped in cold-start mode) -----------------------

REBUILD_CONTROL_API=false
REBUILD_FRONTEND=false

if [[ "$COLD_START" == "true" ]]; then
  echo "[dev-stack-auto-rebuild] cold start — forcing rebuild of all services"
  REBUILD_CONTROL_API=true
  REBUILD_FRONTEND=true
else
  CHANGED_FILES="$(git diff --name-only "$PREV_SHA" "$NEW_SHA" 2>/dev/null || true)"

  if [[ -z "$CHANGED_FILES" ]]; then
    echo "[dev-stack-auto-rebuild] no changed files — nothing to do"
    log_record "skipped" "" "[]" "no changed files"
    exit 0
  fi

  while IFS= read -r f; do
    case "$f" in
      # control-api source: Rust crates, Dockerfiles, migrations, proto
      services/control-api/*|crates/*|Cargo.toml|Cargo.lock|docker/control-api/*|migrations/*|proto/*)
        REBUILD_CONTROL_API=true ;;
      # frontend source
      frontend/*|docker/frontend/*)
        REBUILD_FRONTEND=true ;;
      # compose config changes affect all built services
      compose/dev.yml|compose/full.yml|compose/tailscale.yml|compose/tailscale.env|compose/scripts/*)
        REBUILD_CONTROL_API=true
        REBUILD_FRONTEND=true ;;
    esac
  done <<< "$CHANGED_FILES"

  if [[ "$REBUILD_CONTROL_API" == "false" && "$REBUILD_FRONTEND" == "false" ]]; then
    echo "[dev-stack-auto-rebuild] no service paths changed — skipping"
    log_record "skipped" "" "[]" "no service paths changed"
    exit 0
  fi
fi

# -- Build -------------------------------------------------------------------

SERVICES_REBUILT=()

COMPOSE_CMD="${COMPOSE_CMD:-docker compose -f $REPO_ROOT/compose/dev.yml}"

post_gh_status "pending" "Dev-stack rebuild in progress"

if [[ "$REBUILD_CONTROL_API" == "true" ]]; then
  echo "[dev-stack-auto-rebuild] building control-api..."
  if ! $COMPOSE_CMD build control-api 2>&1; then
    echo "[dev-stack-auto-rebuild] control-api build FAILED"
    log_record "build_failed" "" '["control-api"]' "control-api build error"
    post_gh_status "failure" "control-api build failed"
    exit 1
  fi
  SERVICES_REBUILT+=(control-api)
fi

if [[ "$REBUILD_FRONTEND" == "true" ]]; then
  echo "[dev-stack-auto-rebuild] building frontend..."
  if ! $COMPOSE_CMD build frontend 2>&1; then
    echo "[dev-stack-auto-rebuild] frontend build FAILED"
    log_record "build_failed" "" '["frontend"]' "frontend build error"
    post_gh_status "failure" "frontend build failed"
    exit 1
  fi
  SERVICES_REBUILT+=(frontend)
fi

# -- Restart -----------------------------------------------------------------

if [[ "$COLD_START" == "true" ]]; then
  # Cold start: run migrations then bring up the entire stack (infra + app services).
  # --no-deps is intentionally omitted so dependencies (postgres, neo4j, qdrant, etc.) start too.
  echo "[dev-stack-auto-rebuild] cold start: running migrations..."
  if ! $COMPOSE_CMD up --force-recreate rb-migrations 2>&1; then
    echo "[dev-stack-auto-rebuild] rb-migrations FAILED"
    log_record "restart_failed" "" "$(python3 -c "import json,sys; print(json.dumps(sys.argv[1].split()))" "${SERVICES_REBUILT[*]}")" "cold start migrations failed"
    post_gh_status "failure" "Cold start rb-migrations failed"
    exit 1
  fi
  echo "[dev-stack-auto-rebuild] cold start: bringing up full stack..."
  if ! $COMPOSE_CMD up -d 2>&1; then
    echo "[dev-stack-auto-rebuild] full stack up FAILED"
    log_record "restart_failed" "" "$(python3 -c "import json,sys; print(json.dumps(sys.argv[1].split()))" "${SERVICES_REBUILT[*]}")" "cold start compose up -d error"
    post_gh_status "failure" "Cold start compose up -d failed"
    exit 1
  fi
else
  if [[ "$REBUILD_CONTROL_API" == "true" ]]; then
    echo "[dev-stack-auto-rebuild] re-running migrations..."
    # Blocks until rb-migrations exits (restart: "no"). Idempotent — skips applied versions.
    if ! $COMPOSE_CMD up --force-recreate rb-migrations 2>&1; then
      echo "[dev-stack-auto-rebuild] rb-migrations FAILED"
      log_record "restart_failed" "" "$(python3 -c "import json,sys; print(json.dumps(sys.argv[1].split()))" "${SERVICES_REBUILT[*]}")" "migrations failed"
      post_gh_status "failure" "rb-migrations failed"
      exit 1
    fi

    echo "[dev-stack-auto-rebuild] restarting control-api..."
    if ! $COMPOSE_CMD up -d --no-deps --force-recreate control-api 2>&1; then
      echo "[dev-stack-auto-rebuild] control-api restart FAILED"
      log_record "restart_failed" "" "$(python3 -c "import json,sys; print(json.dumps(sys.argv[1].split()))" "${SERVICES_REBUILT[*]}")" "control-api compose up error"
      post_gh_status "failure" "control-api restart failed"
      exit 1
    fi
  fi

  if [[ "$REBUILD_FRONTEND" == "true" ]]; then
    echo "[dev-stack-auto-rebuild] restarting frontend..."
    if ! $COMPOSE_CMD up -d --no-deps --force-recreate frontend 2>&1; then
      echo "[dev-stack-auto-rebuild] frontend restart FAILED"
      log_record "restart_failed" "" "$(python3 -c "import json,sys; print(json.dumps(sys.argv[1].split()))" "${SERVICES_REBUILT[*]}")" "frontend compose up error"
      post_gh_status "failure" "frontend restart failed"
      exit 1
    fi
  fi
fi

# -- Health check ------------------------------------------------------------

echo "[dev-stack-auto-rebuild] waiting 15s for services to stabilise..."
sleep 15

HEALTH_OK=true
HEALTH_DETAIL=""

if [[ "$REBUILD_CONTROL_API" == "true" ]]; then
  PORT="${CONTROL_API_HOST_PORT:-8080}"
  HTTP_CODE="$(curl -s -o /tmp/rb-health-check.json -w "%{http_code}" \
    --max-time 10 "http://localhost:${PORT}/health" 2>/dev/null || echo "000")"
  if [[ "$HTTP_CODE" == "200" ]]; then
    HEALTH_DETAIL="${HEALTH_DETAIL}control-api=ok "
  else
    HEALTH_OK=false
    HEALTH_DETAIL="${HEALTH_DETAIL}control-api=FAIL(${HTTP_CODE}) "
    echo "[dev-stack-auto-rebuild] control-api health check failed: HTTP $HTTP_CODE"
  fi
fi

if [[ "$REBUILD_FRONTEND" == "true" ]]; then
  PORT="${FRONTEND_HOST_PORT:-15173}"
  HTTP_CODE="$(curl -s -o /dev/null -w "%{http_code}" \
    --max-time 10 "http://localhost:${PORT}/" 2>/dev/null || echo "000")"
  if [[ "$HTTP_CODE" == "200" ]]; then
    HEALTH_DETAIL="${HEALTH_DETAIL}frontend=ok "
  else
    HEALTH_OK=false
    HEALTH_DETAIL="${HEALTH_DETAIL}frontend=FAIL(${HTTP_CODE}) "
    echo "[dev-stack-auto-rebuild] frontend health check failed: HTTP $HTTP_CODE"
  fi
fi

REBUILT_JSON="$(python3 -c "import json,sys; print(json.dumps(sys.argv[1].split()))" "${SERVICES_REBUILT[*]}")"
HEALTH_DETAIL="${HEALTH_DETAIL% }"  # trim trailing space

if [[ "$HEALTH_OK" == "true" ]]; then
  echo "[dev-stack-auto-rebuild] all healthy: $HEALTH_DETAIL"
  log_record "ok" "$HEALTH_DETAIL" "$REBUILT_JSON" ""
  post_gh_status "success" "Dev-stack healthy after rebuild: $HEALTH_DETAIL"
else
  echo "[dev-stack-auto-rebuild] UNHEALTHY after rebuild: $HEALTH_DETAIL"
  log_record "health_failed" "$HEALTH_DETAIL" "$REBUILT_JSON" "health check failed"
  post_gh_status "failure" "Dev-stack unhealthy after rebuild: $HEALTH_DETAIL"
  exit 1
fi
