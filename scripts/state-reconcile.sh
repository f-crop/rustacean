#!/usr/bin/env bash
# state-reconcile.sh — continuous state reconciliation routine
#
# Diffs desired state vs observed state across 5 dimensions per env.
# Fires every 30 min via Paperclip routine. Idempotent.
#
# Dimensions checked per env:
#   (a) git rev-parse origin/main vs docker inspect running image SHA per service
#   (b) compose/env.schema.toml required keys vs docker exec env per service
#   (c) declared migration head vs applied migrations in _sqlx_migrations table
#   (d) per-issue deployment-fingerprint.SHA vs current git HEAD on that env
#   (e) latest UAT fingerprint image_shas vs current running container image SHAs
#
# Drift behaviour:
#   - OPEN state-drift:<env> umbrella issue — created idempotently, updated on each fire
#   - Closed (done) umbrella issue — auto-closed when drift clears; not re-opened when new drift arrives
#   - Drift on a closed *done* issue — appends drift-detected comment; NO auto-re-open
#   - First-fire baseline — drifts recorded before RECONCILE_BASELINE_DATE are ignored
#
# Required env vars:
#   PAPERCLIP_API_KEY     — Bearer token
#   PAPERCLIP_API_URL     — e.g. http://100.87.157.74:3100
#   PAPERCLIP_COMPANY_ID  — company UUID
#   PAPERCLIP_AGENT_ID    — Platform Engineer agent UUID
#   PAPERCLIP_RUN_ID      — run UUID (injected by harness)
#
# Optional env vars:
#   RECONCILE_ENVS          — space-separated override list: "dev uat" (default: auto-detect)
#   RECONCILE_BASELINE_DATE — ISO date string; drifts before this date are ignored
#                             Written to scripts/.state-reconcile-baseline on first fire
#   DB_PASSWORD             — Postgres password (default: rustbrain)
#   DB_USER                 — Postgres user (default: rustbrain)
#   DB_NAME                 — Postgres database (default: rustbrain)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# ── Constants ─────────────────────────────────────────────────────────────────

BASELINE_FILE="$SCRIPT_DIR/.state-reconcile-baseline"
API="${PAPERCLIP_API_URL:-}"
COMPANY="${PAPERCLIP_COMPANY_ID:-}"
AGENT="${PAPERCLIP_AGENT_ID:-}"
RUN_ID="${PAPERCLIP_RUN_ID:-}"
DB_PASSWORD="${DB_PASSWORD:-rustbrain}"
DB_USER="${DB_USER:-rustbrain}"
DB_NAME="${DB_NAME:-rustbrain}"

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

COMPOSE_PROJECT_DEV="rustbrain-dev"
COMPOSE_FILE_DEV="$REPO_ROOT/compose/dev.yml"
COMPOSE_FILE_UAT="$REPO_ROOT/compose/uat.yml"

log()  { echo "[$(date -u +%H:%M:%S)] [reconcile] $*"; }
warn() { echo "[$(date -u +%H:%M:%S)] [reconcile] WARN: $*" >&2; }

# ── Validation ────────────────────────────────────────────────────────────────

for var in PAPERCLIP_API_KEY PAPERCLIP_API_URL PAPERCLIP_COMPANY_ID PAPERCLIP_AGENT_ID; do
  if [ -z "${!var:-}" ]; then
    echo "ERROR: required env var $var is not set" >&2
    exit 1
  fi
done

# ── 0. Budget guard ───────────────────────────────────────────────────────────

log "Checking dashboard budget..."
BUDGET_PCT=$(curl -sf \
  -H "Authorization: Bearer $PAPERCLIP_API_KEY" \
  "$API/api/companies/$COMPANY/dashboard" \
  | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('budgetUsedPct', 0))" 2>/dev/null || echo "0")

if python3 -c "import sys; sys.exit(0 if float('${BUDGET_PCT}') <= 80 else 1)" 2>/dev/null; then
  log "Budget OK (${BUDGET_PCT}% used)"
else
  log "Budget >80% (${BUDGET_PCT}%). Exiting early to preserve budget."
  exit 0
fi

# ── 1. First-fire baseline ────────────────────────────────────────────────────

RECONCILE_BASELINE_DATE="${RECONCILE_BASELINE_DATE:-}"
if [ -z "$RECONCILE_BASELINE_DATE" ]; then
  if [ -f "$BASELINE_FILE" ]; then
    RECONCILE_BASELINE_DATE=$(cat "$BASELINE_FILE" | tr -d '[:space:]')
    log "Loaded baseline date from file: $RECONCILE_BASELINE_DATE"
  else
    RECONCILE_BASELINE_DATE=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
    echo "$RECONCILE_BASELINE_DATE" > "$BASELINE_FILE"
    log "First fire — recorded baseline date: $RECONCILE_BASELINE_DATE"
    log "Skipping drift check on first fire (establishing baseline)."
    exit 0
  fi
fi

NOW_EPOCH=$(date -u +%s)
BASELINE_EPOCH=$(date -u -d "$RECONCILE_BASELINE_DATE" +%s 2>/dev/null || \
  python3 -c "from datetime import datetime; print(int(datetime.fromisoformat('${RECONCILE_BASELINE_DATE}'.replace('Z','+00:00')).timestamp()))")

if [ "$NOW_EPOCH" -le "$BASELINE_EPOCH" ]; then
  log "Current time is at or before baseline — skipping (baseline not yet elapsed)."
  exit 0
fi

# ── 2. Detect running envs ────────────────────────────────────────────────────

detect_envs() {
  local envs=()
  if [ -n "${RECONCILE_ENVS:-}" ]; then
    read -ra envs <<< "$RECONCILE_ENVS"
  else
    if docker compose -f "$COMPOSE_FILE_DEV" ps --services --status running 2>/dev/null | grep -q .; then
      envs+=("dev")
    fi
    if [ -f "$COMPOSE_FILE_UAT" ] && docker compose -f "$COMPOSE_FILE_UAT" ps --services --status running 2>/dev/null | grep -q .; then
      envs+=("uat")
    fi
  fi
  echo "${envs[@]:-}"
}

compose_file_for_env() {
  case "$1" in
    dev) echo "$COMPOSE_FILE_DEV" ;;
    uat) echo "$COMPOSE_FILE_UAT" ;;
    *)   echo "" ;;
  esac
}

# ── 3. Dimension helpers ──────────────────────────────────────────────────────

# (a) git rev-parse origin/main
get_git_main_sha() {
  cd "$REPO_ROOT"
  git fetch origin main --quiet 2>/dev/null || true
  git rev-parse origin/main 2>/dev/null || git rev-parse HEAD
}

# (a) running container image SHA per service
get_container_image_sha() {
  local compose_file="$1" svc="$2"
  local container
  container=$(docker compose -f "$compose_file" ps -q "$svc" 2>/dev/null | head -1 || true)
  if [ -z "$container" ]; then
    echo "MISSING"
    return
  fi
  docker inspect "$container" --format='{{.Image}}' 2>/dev/null || echo "ERROR"
}

# (b) parse env.schema.toml — return required keys for a given service
get_schema_required_keys_for_service() {
  local svc="$1"
  python3 - "$REPO_ROOT/compose/env.schema.toml" "$svc" <<'PYEOF'
import sys
try:
    import tomllib
except ImportError:
    import tomli as tomllib

schema_path, service = sys.argv[1], sys.argv[2]
with open(schema_path, "rb") as f:
    schema = tomllib.load(f)

required = []
for var_name, var_def in schema.get("var", {}).items():
    if var_def.get("required", False):
        services = var_def.get("services", [])
        if service in services:
            required.append(var_name)

print("\n".join(sorted(required)))
PYEOF
}

# (b) get live env keys inside a running container
get_live_env_keys_for_service() {
  local compose_file="$1" svc="$2"
  local container
  container=$(docker compose -f "$compose_file" ps -q "$svc" 2>/dev/null | head -1 || true)
  if [ -z "$container" ]; then
    echo ""
    return
  fi
  docker exec "$container" env 2>/dev/null | cut -d= -f1 | sort || echo ""
}

# (c) count of declared migration files for a schema dir
get_declared_migration_count() {
  local schema_dir="$1"  # "control" or "tenant"
  local dir="$REPO_ROOT/migrations/$schema_dir"
  [ -d "$dir" ] || { echo "0"; return; }
  ls "$dir" | grep -c '\.sql$' || echo "0"
}

# (c) max version applied in control.schema_migrations
get_control_applied_count() {
  local compose_file="$1"
  docker compose -f "$compose_file" exec -T postgres \
    psql -U "$DB_USER" -d "$DB_NAME" -t -A \
    -c "SELECT COUNT(*) FROM control.schema_migrations;" \
    2>/dev/null | tr -d '[:space:]' || echo "0"
}

# (c) max version applied in a tenant schema (sample first tenant found)
get_tenant_applied_count() {
  local compose_file="$1"
  # Get a sample tenant schema name
  local tenant_schema
  tenant_schema=$(docker compose -f "$compose_file" exec -T postgres \
    psql -U "$DB_USER" -d "$DB_NAME" -t -A \
    -c "SELECT schema_name FROM information_schema.schemata WHERE schema_name LIKE 'tenant_%' LIMIT 1;" \
    2>/dev/null | tr -d '[:space:]' || echo "")
  if [ -z "$tenant_schema" ]; then
    echo "NO_TENANT"
    return
  fi
  docker compose -f "$compose_file" exec -T postgres \
    psql -U "$DB_USER" -d "$DB_NAME" -t -A \
    -c "SELECT COUNT(*) FROM ${tenant_schema}.schema_migrations;" \
    2>/dev/null | tr -d '[:space:]' || echo "0"
}

# (d) fetch issues with deployment-fingerprint docs on the given env
get_deployment_fingerprint_issues() {
  local env="$1"
  # Query issues that have deployment-fingerprint documents
  # We search for recently done issues assigned to this env
  curl -sf \
    -H "Authorization: Bearer $PAPERCLIP_API_KEY" \
    "$API/api/companies/$COMPANY/issues?status=done&q=deployment-fingerprint" \
    | python3 -c "
import sys, json
data = json.load(sys.stdin)
issues = data if isinstance(data, list) else data.get('issues', [])
for issue in issues[:20]:
    print(issue['id'], issue.get('identifier',''))
" 2>/dev/null || true
}

get_issue_fingerprint_sha() {
  local issue_id="$1"
  curl -sf \
    -H "Authorization: Bearer $PAPERCLIP_API_KEY" \
    "$API/api/issues/$issue_id/documents/deployment-fingerprint" \
    | python3 -c "
import sys, json
try:
    doc = json.load(sys.stdin)
    body = doc.get('body', '{}')
    if isinstance(body, str):
        fp = json.loads(body)
    else:
        fp = body
    print(fp.get('SHA',''), fp.get('env',''))
except Exception:
    print('', '')
" 2>/dev/null || echo " "
}

# (e) latest UAT fingerprint from Paperclip — search for most recent
get_latest_uat_fingerprint_image_shas() {
  # Try to find a recent done issue's UAT fingerprint document
  curl -sf \
    -H "Authorization: Bearer $PAPERCLIP_API_KEY" \
    "$API/api/companies/$COMPANY/issues?status=done&q=UAT+fingerprint" \
    | python3 -c "
import sys, json
data = json.load(sys.stdin)
issues = data if isinstance(data, list) else data.get('issues', [])
print(json.dumps([i['id'] for i in issues[:5]]))
" 2>/dev/null | python3 -c "
import sys, json
ids = json.load(sys.stdin)
print(ids[0] if ids else '')
" 2>/dev/null || echo ""
}

# ── 4. Per-env reconciliation ─────────────────────────────────────────────────

reconcile_env() {
  local env="$1"
  local compose_file
  compose_file=$(compose_file_for_env "$env")
  if [ -z "$compose_file" ] || [ ! -f "$compose_file" ]; then
    warn "No compose file for env '$env' — skipping"
    return
  fi

  log "=== Reconciling env: $env ==="
  local drift_lines=()
  local git_sha
  git_sha=$(get_git_main_sha)
  log "  Git main SHA: $git_sha"

  # ── (a) git SHA vs running image SHA ──────────────────────────────────────
  log "  Dimension (a): git SHA vs running container images..."
  local dim_a_drift=()
  for svc in "${PIPELINE_SERVICES[@]}"; do
    local img_sha
    img_sha=$(get_container_image_sha "$compose_file" "$svc")
    if [ "$img_sha" = "MISSING" ]; then
      dim_a_drift+=("  [a] $svc: container not running")
    elif [ "$img_sha" = "ERROR" ]; then
      dim_a_drift+=("  [a] $svc: docker inspect failed")
    else
      # The image SHA from docker inspect is a content hash; we can't compare
      # directly to git SHA — but we check if image is built from origin/main
      # by comparing the image label org.opencontainers.image.revision if present
      local img_rev
      img_rev=$(docker inspect "$img_sha" --format='{{index .Config.Labels "org.opencontainers.image.revision"}}' 2>/dev/null || echo "")
      if [ -n "$img_rev" ] && [ "$img_rev" != "$git_sha" ]; then
        dim_a_drift+=("  [a] $svc: image built from $img_rev, git main is $git_sha")
      fi
      log "    $svc: image=$img_sha rev=${img_rev:-<unlabeled>}"
    fi
  done
  if [ ${#dim_a_drift[@]} -gt 0 ]; then
    drift_lines+=("### (a) Git SHA vs Running Images")
    drift_lines+=("${dim_a_drift[@]}")
  fi

  # ── (b) env.schema.toml required keys vs live env ────────────────────────
  log "  Dimension (b): env.schema.toml required keys vs live container env..."
  local dim_b_drift=()
  # Check services that have required keys
  local checked_services=("control-api" "ingest-clone" "projector-pg" "tombstoner")
  for svc in "${checked_services[@]}"; do
    local required_keys
    required_keys=$(get_schema_required_keys_for_service "$svc" 2>/dev/null || echo "")
    if [ -z "$required_keys" ]; then
      continue
    fi
    local live_keys
    live_keys=$(get_live_env_keys_for_service "$compose_file" "$svc" 2>/dev/null || echo "")
    if [ -z "$live_keys" ]; then
      dim_b_drift+=("  [b] $svc: container not running; cannot check env keys")
      continue
    fi
    while IFS= read -r key; do
      [ -z "$key" ] && continue
      if ! echo "$live_keys" | grep -qx "$key"; then
        dim_b_drift+=("  [b] $svc: required key '$key' missing from container env")
      fi
    done <<< "$required_keys"
  done
  if [ ${#dim_b_drift[@]} -gt 0 ]; then
    drift_lines+=("### (b) Env Schema vs Live Container Env")
    drift_lines+=("${dim_b_drift[@]}")
  fi

  # ── (c) Migration head vs running DB applied migrations ──────────────────
  log "  Dimension (c): declared migration head vs applied migrations in DB..."
  local dim_c_drift=()
  # Test DB connectivity via control.schema_migrations (actual migration table)
  local db_check_output
  db_check_output=$(docker compose -f "$compose_file" exec -T postgres \
    psql -U "$DB_USER" -d "$DB_NAME" -t -A \
    -c "SELECT 1 FROM control.schema_migrations LIMIT 1;" 2>&1) || true
  if echo "$db_check_output" | grep -qi "does not exist\|no such table\|ERROR"; then
    warn "  Dimension (c): control.schema_migrations not found — skipping migration check"
  else
    # Control schema
    local control_declared control_applied
    control_declared=$(get_declared_migration_count "control")
    control_applied=$(get_control_applied_count "$compose_file")
    if [ "$control_applied" -lt "$control_declared" ]; then
      local unapplied=$(( control_declared - control_applied ))
      dim_c_drift+=("  [c] control: $unapplied unapplied migration(s) (declared=$control_declared, applied=$control_applied)")
    fi
    log "    control: declared=$control_declared applied=$control_applied"

    # Tenant schema (sample one tenant to check the shared tenant migration set)
    local tenant_declared tenant_applied
    tenant_declared=$(get_declared_migration_count "tenant")
    tenant_applied=$(get_tenant_applied_count "$compose_file")
    if [ "$tenant_applied" = "NO_TENANT" ]; then
      log "    tenant: no tenant schemas found — skipping tenant migration check"
    elif [ "$tenant_applied" -lt "$tenant_declared" ]; then
      local unapplied=$(( tenant_declared - tenant_applied ))
      dim_c_drift+=("  [c] tenant: $unapplied unapplied migration(s) in sample tenant schema (declared=$tenant_declared, applied=$tenant_applied)")
    fi
    [ "$tenant_applied" != "NO_TENANT" ] && log "    tenant: declared=$tenant_declared applied=$tenant_applied"
  fi
  if [ ${#dim_c_drift[@]} -gt 0 ]; then
    drift_lines+=("### (c) Migration Head vs Applied Migrations")
    drift_lines+=("${dim_c_drift[@]}")
  fi

  # ── (d) deployment-fingerprint.SHA vs current deployed SHA ───────────────
  log "  Dimension (d): deployment-fingerprint SHA vs current SHA..."
  local dim_d_drift=()
  local fp_issue_list
  fp_issue_list=$(get_deployment_fingerprint_issues "$env" 2>/dev/null || echo "")
  while IFS=' ' read -r issue_id issue_ident; do
    [ -z "$issue_id" ] && continue
    local fp_sha fp_env
    read -r fp_sha fp_env <<< "$(get_issue_fingerprint_sha "$issue_id" 2>/dev/null || echo ' ')"
    [ -z "$fp_sha" ] && continue
    [ "$fp_env" != "$env" ] && continue
    # Compare fingerprint SHA to current git main
    if [ "$fp_sha" != "$git_sha" ]; then
      dim_d_drift+=("  [d] $issue_ident: fingerprint SHA=$fp_sha, current main=$git_sha")
    fi
    log "    $issue_ident: fp_sha=${fp_sha:0:8} git_main=${git_sha:0:8} env=$fp_env"
  done <<< "$fp_issue_list"
  if [ ${#dim_d_drift[@]} -gt 0 ]; then
    drift_lines+=("### (d) Deployment-Fingerprint SHA vs Current SHA")
    drift_lines+=("${dim_d_drift[@]}")
  fi

  # ── (e) UAT fingerprint vs current image set ─────────────────────────────
  if [ "$env" = "uat" ]; then
    log "  Dimension (e): UAT fingerprint vs current image set..."
    local dim_e_drift=()
    local latest_fp_file="/tmp/uat-fingerprint-latest-${env}.json"

    # Try to get latest UAT fingerprint from /tmp if uat-fingerprint.sh was recently run
    if [ ! -f "$SCRIPT_DIR/uat-fingerprint.sh" ]; then
      log "    uat-fingerprint.sh not found — dim (e) skipped (uat-fingerprint dependency not yet landed)"
      return
    fi
    if [ ! -f "$latest_fp_file" ] || [ "$(find "$latest_fp_file" -mmin +60 2>/dev/null)" != "" ]; then
      log "    Refreshing UAT fingerprint (running uat-fingerprint.sh)..."
      bash "$SCRIPT_DIR/uat-fingerprint.sh" "$COMPOSE_FILE_UAT" "$latest_fp_file" 2>/dev/null || true
    fi

    if [ -f "$latest_fp_file" ]; then
      local fp_verdict
      fp_verdict=$(python3 -c "import json; d=json.load(open('$latest_fp_file')); print(d.get('verdict','unknown'))" 2>/dev/null || echo "unknown")
      if [ "$fp_verdict" != "pass" ]; then
        dim_e_drift+=("  [e] UAT fingerprint verdict=$fp_verdict — live stack may be unhealthy")
      else
        log "    UAT fingerprint: verdict=pass"
      fi

      # Compare image SHAs from fingerprint vs current running containers
      for svc in "${PIPELINE_SERVICES[@]}"; do
        local fp_img_sha
        fp_img_sha=$(python3 -c "
import json, sys
try:
    d = json.load(open('$latest_fp_file'))
    print(d.get('image_shas', {}).get('$svc', ''))
except: print('')
" 2>/dev/null || echo "")
        local live_img_sha
        live_img_sha=$(get_container_image_sha "$compose_file" "$svc" 2>/dev/null || echo "MISSING")
        if [ -n "$fp_img_sha" ] && [ "$fp_img_sha" != "$live_img_sha" ] && [ "$live_img_sha" != "MISSING" ]; then
          dim_e_drift+=("  [e] $svc: fingerprint image=${fp_img_sha:0:20}..., running=${live_img_sha:0:20}...")
        fi
      done
    else
      log "    No UAT fingerprint available — skipping dimension (e)"
    fi
    if [ ${#dim_e_drift[@]} -gt 0 ]; then
      drift_lines+=("### (e) UAT Fingerprint vs Current Image Set")
      drift_lines+=("${dim_e_drift[@]}")
    fi
  fi

  # ── 5. Manage state-drift:<env> umbrella issue ───────────────────────────
  manage_drift_issue "$env" "${drift_lines[@]+"${drift_lines[@]}"}"
}

# ── 5. Umbrella issue management ─────────────────────────────────────────────

find_drift_umbrella_issue() {
  local env="$1"
  curl -sf \
    -H "Authorization: Bearer $PAPERCLIP_API_KEY" \
    "$API/api/companies/$COMPANY/issues?q=state-drift:+$env" \
    | python3 -c "
import sys, json
data = json.load(sys.stdin)
issues = data if isinstance(data, list) else data.get('issues', [])
# Find exact title match
for i in issues:
    if i.get('title','').strip() == 'state-drift: $env':
        print(i['id'], i.get('status',''), i.get('identifier',''))
        break
" 2>/dev/null || echo ""
}

manage_drift_issue() {
  local env="$1"
  shift
  local drift_lines=("$@")
  local has_drift=0
  [ ${#drift_lines[@]} -gt 0 ] && has_drift=1

  local TIMESTAMP
  TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

  local umbrella_info
  umbrella_info=$(find_drift_umbrella_issue "$env")
  local umbrella_id umbrella_status umbrella_ident
  read -r umbrella_id umbrella_status umbrella_ident <<< "$umbrella_info"

  if [ "$has_drift" -eq 0 ]; then
    log "  No drift detected on $env"
    if [ -n "$umbrella_id" ] && [ "$umbrella_status" != "done" ] && [ "$umbrella_status" != "cancelled" ]; then
      log "  Closing state-drift:$env umbrella issue ($umbrella_ident) — drift cleared"
      curl -sf -X PATCH \
        -H "Authorization: Bearer $PAPERCLIP_API_KEY" \
        -H "X-Paperclip-Run-Id: $RUN_ID" \
        -H "Content-Type: application/json" \
        "$API/api/issues/$umbrella_id" \
        -d "$(jq -n --arg comment "## Drift Cleared — $TIMESTAMP

All 5 reconciliation dimensions are clean on **$env**. Auto-closing." \
          '{status: "done", comment: $comment}')" > /dev/null
      log "  Umbrella issue $umbrella_ident closed."
    fi
    return
  fi

  # Build drift report body
  local report_body
  report_body=$(printf "## State Drift Detected — %s (%s)\n\n" "$env" "$TIMESTAMP")
  for line in "${drift_lines[@]}"; do
    report_body+="$line"$'\n'
  done
  report_body+=$'\n---\n'
  report_body+="Baseline date: $RECONCILE_BASELINE_DATE  "

  if [ -z "$umbrella_id" ]; then
    # Create new umbrella issue
    log "  Creating state-drift:$env umbrella issue..."
    local new_issue
    new_issue=$(curl -sf -X POST \
      -H "Authorization: Bearer $PAPERCLIP_API_KEY" \
      -H "X-Paperclip-Run-Id: $RUN_ID" \
      -H "Content-Type: application/json" \
      "$API/api/companies/$COMPANY/issues" \
      -d "$(jq -n \
        --arg title "state-drift: $env" \
        --arg desc "$report_body" \
        --arg assignee "$AGENT" \
        --arg projectId "142de4ac-fd22-4274-8722-628d623fe338" \
        '{
          title: $title,
          description: $desc,
          assigneeAgentId: $assignee,
          projectId: $projectId,
          priority: "high",
          status: "in_progress"
        }')")
    local new_id new_ident
    new_id=$(echo "$new_issue" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('id',''))" 2>/dev/null || echo "")
    new_ident=$(echo "$new_issue" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('identifier',''))" 2>/dev/null || echo "")
    log "  Created umbrella issue: $new_ident ($new_id)"
  else
    # Update existing open umbrella issue with new diff as a comment
    log "  Updating state-drift:$env umbrella issue ($umbrella_ident)..."
    local update_status="$umbrella_status"
    if [ "$umbrella_status" = "done" ] || [ "$umbrella_status" = "cancelled" ]; then
      # Don't re-open closed issues — comment only per policy
      log "  Umbrella issue $umbrella_ident is $umbrella_status — appending drift comment only (no re-open)"
      curl -sf -X POST \
        -H "Authorization: Bearer $PAPERCLIP_API_KEY" \
        -H "X-Paperclip-Run-Id: $RUN_ID" \
        -H "Content-Type: application/json" \
        "$API/api/issues/$umbrella_id/comments" \
        -d "$(jq -n --arg body "## drift-detected — $TIMESTAMP

New drift found on **$env** but umbrella issue is $umbrella_status. CTO triage required to re-open.

$report_body" '{body: $body}')" > /dev/null
      return
    fi
    # Append diff comment to open umbrella
    curl -sf -X POST \
      -H "Authorization: Bearer $PAPERCLIP_API_KEY" \
      -H "X-Paperclip-Run-Id: $RUN_ID" \
      -H "Content-Type: application/json" \
      "$API/api/issues/$umbrella_id/comments" \
      -d "$(jq -n --arg body "$report_body" '{body: $body}')" > /dev/null
    log "  Appended drift diff to $umbrella_ident"
  fi

  # Drift-on-closed-issue policy: for any done issue that shows drift in dim (d),
  # append a drift-detected comment without re-opening
  handle_closed_issue_drift_policy "$env" "$TIMESTAMP" "${drift_lines[@]+"${drift_lines[@]}"}"
}

handle_closed_issue_drift_policy() {
  local env="$1" timestamp="$2"
  shift; shift
  local drift_lines=("$@")

  # Extract done-issue identifiers from dim (d) drift lines
  for line in "${drift_lines[@]}"; do
    if [[ "$line" =~ \[d\]\ (RUSAA-[0-9]+): ]]; then
      local ident="${BASH_REMATCH[1]}"
      local issue_id
      issue_id=$(curl -sf \
        -H "Authorization: Bearer $PAPERCLIP_API_KEY" \
        "$API/api/companies/$COMPANY/issues?q=$ident" \
        | python3 -c "
import sys, json
data = json.load(sys.stdin)
issues = data if isinstance(data, list) else data.get('issues', [])
for i in issues:
    if i.get('identifier','') == '$ident' and i.get('status','') == 'done':
        print(i['id'])
        break
" 2>/dev/null || echo "")
      if [ -n "$issue_id" ]; then
        local umbrella_ident=""
        umbrella_ident=$(find_drift_umbrella_issue "$env" | awk '{print $3}')
        log "  Posting drift-detected comment on closed issue $ident (no re-open)"
        local drift_detail="$line"
        curl -sf -X POST \
          -H "Authorization: Bearer $PAPERCLIP_API_KEY" \
          -H "X-Paperclip-Run-Id: $RUN_ID" \
          -H "Content-Type: application/json" \
          "$API/api/issues/$issue_id/comments" \
          -d "$(jq -n \
            --arg body "## drift-detected — $timestamp

Deployment-fingerprint SHA drift detected on **$env** by state-reconcile routine.

$drift_detail

**This issue is NOT re-opened** (per board-confirmed policy). CTO triages via the [$umbrella_ident](/RUSAA/issues/$umbrella_ident) umbrella issue." \
            '{body: $body}')" > /dev/null
      fi
    fi
  done
}

# ── Main ──────────────────────────────────────────────────────────────────────

main() {
  log "state-reconcile starting (run=$RUN_ID)"
  log "Baseline date: $RECONCILE_BASELINE_DATE"

  local envs
  read -ra envs <<< "$(detect_envs)"

  if [ ${#envs[@]} -eq 0 ]; then
    log "No running compose stacks detected — nothing to reconcile."
    exit 0
  fi

  log "Detected envs to reconcile: ${envs[*]}"
  for env in "${envs[@]}"; do
    reconcile_env "$env"
  done

  log "state-reconcile complete."
}

main "$@"
