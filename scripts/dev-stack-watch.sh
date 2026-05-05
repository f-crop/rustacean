#!/usr/bin/env bash
# Polls origin/main for new commits and triggers dev-stack-auto-rebuild.sh on changes.
# Designed to run as a persistent systemd service on mars.
#
# Usage:
#   scripts/dev-stack-watch.sh [REPO_PATH]
#
# Environment:
#   POLL_INTERVAL   Seconds between git fetch polls (default: 60)
#   REMOTE          Remote name to poll (default: origin)
#   BRANCH          Branch to track (default: main)
#   COMPOSE_CMD     Used locally to detect running containers; also passed through to dev-stack-auto-rebuild.sh
#
# Logs: journald captures stdout/stderr when run as a systemd service.
# Rebuild records are written by dev-stack-auto-rebuild.sh to
#   $HOME/.local/state/rustbrain/dev-stack-rebuilds.ndjson
#
# See docs/dev-stack-auto-rebuild.md for setup instructions.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${1:-"$(cd "$SCRIPT_DIR/.." && pwd)"}"
POLL_INTERVAL="${POLL_INTERVAL:-60}"
REMOTE="${REMOTE:-origin}"
BRANCH="${BRANCH:-main}"

REBUILD_SCRIPT="$SCRIPT_DIR/dev-stack-auto-rebuild.sh"

# Resolve COMPOSE_CMD with the same default as dev-stack-auto-rebuild.sh uses.
# This is needed to inspect running container counts for cold-start detection.
COMPOSE_CMD="${COMPOSE_CMD:-docker compose -f $REPO_ROOT/compose/dev.yml}"

# Returns exit code 0 when zero compose services are currently running.
stack_is_cold() {
  local running_ids
  running_ids="$($COMPOSE_CMD ps --quiet --status running 2>/dev/null)" || true
  [[ -z "$running_ids" ]]
}

# Persist LAST_KNOWN_SHA across restarts so commits that land during an outage
# are not silently dropped when the service comes back up.
STATE_DIR="${RB_STATE_DIR:-"$HOME/.local/state/rustbrain"}"
SHA_STATE_FILE="$STATE_DIR/dev-stack-watch-last-sha"
mkdir -p "$STATE_DIR"

echo "[dev-stack-watch] started — polling $REMOTE/$BRANCH every ${POLL_INTERVAL}s"
echo "[dev-stack-watch] repo: $REPO_ROOT"
echo "[dev-stack-watch] rebuild script: $REBUILD_SCRIPT"

cd "$REPO_ROOT"

# Initialise LAST_KNOWN_SHA: restore from state file to catch commits that arrived
# during an outage; fall back to current origin/main only on first run.
git fetch "$REMOTE" "$BRANCH" --quiet 2>/dev/null || \
  echo "[dev-stack-watch] initial fetch failed; will retry on next cycle" >&2
if [[ -f "$SHA_STATE_FILE" ]]; then
  LAST_KNOWN_SHA="$(cat "$SHA_STATE_FILE")"
  echo "[dev-stack-watch] resuming from persisted SHA $LAST_KNOWN_SHA"
else
  LAST_KNOWN_SHA="$(git rev-parse "$REMOTE/$BRANCH" 2>/dev/null || git rev-parse HEAD)"
  echo "$LAST_KNOWN_SHA" > "$SHA_STATE_FILE"
fi

echo "[dev-stack-watch] tracking from $LAST_KNOWN_SHA"

# Cold-start check on startup: if the stack is fully stopped, bring it up immediately
# without waiting for the next commit. This handles the case where the watcher restarts
# after an outage that also stopped the compose stack.
if stack_is_cold; then
  echo "[dev-stack-watch] stack is not running — triggering cold start"
  "$REBUILD_SCRIPT" --cold-start "$LAST_KNOWN_SHA" "$LAST_KNOWN_SHA" || \
    echo "[dev-stack-watch] cold start failed — see rebuild logs for details" >&2
else
  echo "[dev-stack-watch] stack is running — entering poll loop"
fi

while true; do
  sleep "$POLL_INTERVAL"

  if ! git fetch "$REMOTE" "$BRANCH" --quiet 2>/dev/null; then
    echo "[dev-stack-watch] fetch failed — will retry" >&2
    continue
  fi

  NEW_SHA="$(git rev-parse "$REMOTE/$BRANCH")"

  if [[ "$NEW_SHA" == "$LAST_KNOWN_SHA" ]]; then
    continue
  fi

  echo "[dev-stack-watch] new commit: $LAST_KNOWN_SHA → $NEW_SHA"

  # Fast-forward the local branch if it is currently checked out on main.
  CURRENT_BRANCH="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || true)"
  if [[ "$CURRENT_BRANCH" == "$BRANCH" ]]; then
    git merge --ff-only "$REMOTE/$BRANCH" --quiet 2>/dev/null || true
  fi

  PREV_SHA="$LAST_KNOWN_SHA"
  LAST_KNOWN_SHA="$NEW_SHA"
  echo "$LAST_KNOWN_SHA" > "$SHA_STATE_FILE"

  # If the stack is fully stopped, force a cold start so infra services come up too.
  if stack_is_cold; then
    echo "[dev-stack-watch] stack is not running — triggering cold start: $PREV_SHA → $NEW_SHA"
    "$REBUILD_SCRIPT" --cold-start "$PREV_SHA" "$NEW_SHA" || \
      echo "[dev-stack-watch] cold start exited non-zero — see logs for details" >&2
  else
    echo "[dev-stack-watch] triggering rebuild: $PREV_SHA → $NEW_SHA"
    # Never let a rebuild failure crash the watch loop.
    "$REBUILD_SCRIPT" "$PREV_SHA" "$NEW_SHA" || \
      echo "[dev-stack-watch] rebuild exited non-zero — see logs for details" >&2
  fi
done
