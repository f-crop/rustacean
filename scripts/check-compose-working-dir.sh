#!/usr/bin/env bash
# Detects and optionally fixes compose project.working_dir label drift on rustbrain-dev
# containers. Containers whose working_dir label diverges from the canonical repo compose/
# directory have stale bind-mount resolution: paths like ../migrations:/migrations resolve
# relative to the label, so if the original working directory was cleaned up the bind
# fails at container start.
#
# Usage:
#   scripts/check-compose-working-dir.sh [--fix]
#
# Without --fix: print a report and exit non-zero if drift is detected.
# With --fix:    force-recreate all rustbrain-dev containers so Docker re-labels them
#                with the canonical compose working_dir.
#
# Environment:
#   COMPOSE_CMD  Full docker compose invocation (default: docker compose -f <repo>/compose/dev.yml)
#                On mars: "docker compose --env-file compose/tailscale.env -f compose/dev.yml -f compose/tailscale.yml"
#
# See docs/dev-stack-auto-rebuild.md § Reconciling working_dir drift for details.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CANONICAL_DIR="$REPO_ROOT/compose"
COMPOSE_CMD="${COMPOSE_CMD:-docker compose -f "$REPO_ROOT/compose/dev.yml"}"

FIX=false
if [[ "${1:-}" == "--fix" ]]; then
  FIX=true
fi

echo "[check-compose-working-dir] canonical dir: $CANONICAL_DIR"

CONTAINERS="$(docker ps -a \
  --filter "label=com.docker.compose.project=rustbrain-dev" \
  --format "{{.Names}}" 2>/dev/null)" || true

if [[ -z "$CONTAINERS" ]]; then
  echo "[check-compose-working-dir] no rustbrain-dev containers found — nothing to check"
  exit 0
fi

DRIFT_FOUND=false

while IFS= read -r container; do
  WORKING_DIR="$(docker inspect \
    --format '{{index .Config.Labels "com.docker.compose.project.working_dir"}}' \
    "$container" 2>/dev/null)" || WORKING_DIR=""
  if [[ "$WORKING_DIR" != "$CANONICAL_DIR" ]]; then
    echo "[check-compose-working-dir] DRIFT: $container" >&2
    echo "[check-compose-working-dir]   have: $WORKING_DIR" >&2
    echo "[check-compose-working-dir]   want: $CANONICAL_DIR" >&2
    DRIFT_FOUND=true
  else
    echo "[check-compose-working-dir] OK:    $container"
  fi
done <<< "$CONTAINERS"

if [[ "$DRIFT_FOUND" == "false" ]]; then
  echo "[check-compose-working-dir] all containers are aligned with the canonical compose dir"
  exit 0
fi

if [[ "$FIX" == "false" ]]; then
  echo "" >&2
  echo "[check-compose-working-dir] Drift detected. To fix, run:" >&2
  echo "  COMPOSE_CMD=\"$COMPOSE_CMD\" $0 --fix" >&2
  exit 1
fi

echo "[check-compose-working-dir] force-recreating all services to re-anchor working_dir label..."
$COMPOSE_CMD up -d --force-recreate

echo "[check-compose-working-dir] done. Verify with:"
echo "  docker ps -a --filter label=com.docker.compose.project=rustbrain-dev --format '{{.Names}}' \\"
echo "    | xargs -I{} docker inspect --format '{{index .Config.Labels \"com.docker.compose.project.working_dir\"}} {}' {}"
