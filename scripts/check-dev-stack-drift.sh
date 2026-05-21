#!/usr/bin/env bash
# Reports whether all running dev-stack containers are current with origin/main.
# Reads the git_sha LABEL baked into each image (RUSAA-1637) and compares
# against origin/main HEAD.
#
# Usage:
#   scripts/check-dev-stack-drift.sh [--no-fetch]
#
# Options:
#   --no-fetch  Skip 'git fetch origin main'; use the cached remote-tracking ref.
#
# Exit:
#   0   All running custom containers match origin/main HEAD.
#   1   One or more containers are behind HEAD; JSON drift report on stdout.
#   2   Fatal error (fetch failed, docker unavailable, etc.).
#
# JSON drift report shape (stdout on exit 1):
#   {
#     "head_sha": "abc123",
#     "head_commit_age_seconds": 540,
#     "drifted": [
#       { "service": "control-api", "deployed_sha": "def456", "is_ancestor_of_head": true }
#     ],
#     "ok": ["frontend", "agent-runner"]
#   }
#
# Environment:
#   COMPOSE_ENV_FILE  Optional env-file to source (same convention as dev-stack-auto-rebuild.sh).
#   RB_REPO_PATH      Repo root (default: parent directory of this script).
#   GITHUB_TOKEN      If set, posts a 'dev-stack/drift' commit status to GitHub.
#   GITHUB_REPO       Required with GITHUB_TOKEN (e.g. "jarnura/rustacean").

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${RB_REPO_PATH:-"$(cd "$SCRIPT_DIR/.." && pwd)"}"

if [[ -n "${COMPOSE_ENV_FILE:-}" && -f "$COMPOSE_ENV_FILE" ]]; then
  set -a
  # shellcheck source=/dev/null
  source "$COMPOSE_ENV_FILE"
  set +a
fi

# -- Helpers ------------------------------------------------------------------

die() { echo "[check-dev-stack-drift] ERROR: $*" >&2; exit 2; }

post_gh_status() {
  local state="$1" desc="$2" sha="$3"
  [[ -z "${GITHUB_TOKEN:-}" || -z "${GITHUB_REPO:-}" ]] && return 0
  local body
  body="$(python3 -c "
import json, sys
print(json.dumps({'state': sys.argv[1], 'description': sys.argv[2], 'context': 'dev-stack/drift'}))
" "$state" "$desc")"
  curl -s -o /dev/null \
    -H "Authorization: token $GITHUB_TOKEN" \
    -H "Content-Type: application/json" \
    -d "$body" \
    "https://api.github.com/repos/${GITHUB_REPO}/statuses/${sha}" || true
}

# -- Flags --------------------------------------------------------------------

FETCH=true
while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-fetch) FETCH=false ;;
    *) die "Unknown argument: $1" ;;
  esac
  shift
done

# -- Get origin/main HEAD SHA -------------------------------------------------

cd "$REPO_ROOT"

if [[ "$FETCH" == "true" ]]; then
  git fetch origin main --quiet 2>/dev/null || die "git fetch origin main failed"
fi

HEAD_SHA="$(git rev-parse origin/main)"
HEAD_TS="$(git show -s --format=%ct "$HEAD_SHA" 2>/dev/null || echo "0")"
NOW_TS="$(date +%s)"
HEAD_COMMIT_AGE=$(( NOW_TS - HEAD_TS ))

# -- Services -----------------------------------------------------------------
# All services in compose/dev.yml with a build: stanza.
# Container names follow the compose project name 'rustbrain-dev'.

CUSTOM_SERVICES=(
  control-api
  projector-pg
  tombstoner
  ingest-clone
  expand-worker
  parse-worker
  typecheck-worker
  ingest-graph
  projector-neo4j
  embed-worker
  claude-login
  agent-runner
  frontend
)

# -- Inspect each container ---------------------------------------------------
#
# Accumulate tab-separated rows for drifted services: service<TAB>sha<TAB>is_ancestor
# and newline-separated names for ok services.

DRIFTED_ROWS=""
OK_NAMES=""

for svc in "${CUSTOM_SERVICES[@]}"; do
  container="rustbrain-dev-${svc}-1"

  running="$(docker inspect --format '{{.State.Running}}' "$container" 2>/dev/null || true)"
  [[ -z "$running" ]] && continue  # container absent — stack may be partial

  deployed_sha="$(docker inspect --format '{{index .Config.Labels "git_sha"}}' "$container" 2>/dev/null || true)"

  if [[ -z "$deployed_sha" || "$deployed_sha" == "<no value>" ]]; then
    # Image predates the git_sha LABEL; count as drifted with unknown SHA.
    DRIFTED_ROWS="${DRIFTED_ROWS}${svc}	unknown	false
"
    continue
  fi

  if [[ "$deployed_sha" == "$HEAD_SHA" ]]; then
    OK_NAMES="${OK_NAMES}${svc}
"
  else
    is_ancestor=false
    if git merge-base --is-ancestor "$deployed_sha" "$HEAD_SHA" 2>/dev/null; then
      is_ancestor=true
    fi
    DRIFTED_ROWS="${DRIFTED_ROWS}${svc}	${deployed_sha}	${is_ancestor}
"
  fi
done

# -- Build JSON report --------------------------------------------------------

REPORT="$(python3 -c "
import json, sys

head_sha = sys.argv[1]
head_age = int(sys.argv[2])
drifted_raw = sys.argv[3]
ok_raw = sys.argv[4]

drifted = []
for line in drifted_raw.splitlines():
    line = line.strip()
    if not line:
        continue
    parts = line.split('\t')
    drifted.append({
        'service': parts[0],
        'deployed_sha': parts[1],
        'is_ancestor_of_head': parts[2] == 'true',
    })

ok = [s.strip() for s in ok_raw.splitlines() if s.strip()]

print(json.dumps({
    'head_sha': head_sha,
    'head_commit_age_seconds': head_age,
    'drifted': drifted,
    'ok': ok,
}, indent=2))
" "$HEAD_SHA" "$HEAD_COMMIT_AGE" "$DRIFTED_ROWS" "$OK_NAMES")"

# -- Output & exit ------------------------------------------------------------

DRIFTED_COUNT=0
[[ -n "$DRIFTED_ROWS" ]] && DRIFTED_COUNT="$(printf '%s' "$DRIFTED_ROWS" | grep -c $'\t' || true)"

if [[ "$DRIFTED_COUNT" -eq 0 ]]; then
  echo "[check-dev-stack-drift] all services current (head=${HEAD_SHA:0:8})"
  post_gh_status "success" "Dev-stack current against HEAD" "$HEAD_SHA"
  exit 0
else
  echo "$REPORT"
  post_gh_status "failure" "Dev-stack drift: $DRIFTED_COUNT service(s) behind HEAD" "$HEAD_SHA"
  exit 1
fi
