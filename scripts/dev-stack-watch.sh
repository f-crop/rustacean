#!/usr/bin/env bash
# Polls origin/main for new commits and triggers dev-stack-auto-rebuild.sh on changes.
# Designed to run as a persistent systemd service on mars.
#
# Usage:
#   scripts/dev-stack-watch.sh [REPO_PATH]
#
# Environment:
#   POLL_INTERVAL          Seconds between git fetch polls (default: 60)
#   REMOTE                 Remote name to poll (default: origin)
#   BRANCH                 Branch to track (default: main)
#   COMPOSE_CMD            Used locally to detect running containers; also passed through to dev-stack-auto-rebuild.sh
#   COLD_START_MAX_ATTEMPTS  Max cold-start attempts on startup before exiting for systemd retry (default: 5)
#   COLD_START_RETRY_DELAY   Initial seconds between cold-start retry attempts; doubles each attempt (default: 30)
#
# Logs: journald captures stdout/stderr when run as a systemd service.
# Rebuild records are written by dev-stack-auto-rebuild.sh to
#   $HOME/.local/state/rustbrain/dev-stack-rebuilds.ndjson
#
# See docs/dev-stack-auto-rebuild.md for setup instructions.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${1:-"$(cd "$SCRIPT_DIR/.." && pwd)"}"

# Guard: refuse to start when the script or REPO_ROOT resolves under /tmp/.
# Running the watcher from a /tmp/ working directory brands every container with
# a stale project.working_dir label; bind-mounts like ../migrations:/migrations:ro
# break the moment that directory is cleaned up.
if [[ "$SCRIPT_DIR" == /tmp/* || "$REPO_ROOT" == /tmp/* ]]; then
  echo "[dev-stack-watch] FATAL: refusing to run from a /tmp/ path" >&2
  echo "[dev-stack-watch]   SCRIPT_DIR=$SCRIPT_DIR" >&2
  echo "[dev-stack-watch]   REPO_ROOT=$REPO_ROOT" >&2
  echo "[dev-stack-watch]   Run from the canonical repo: ~/projects/rustacean/scripts/dev-stack-watch.sh" >&2
  exit 1
fi

# Guard: compose/active-env must exist and be non-empty. Systemd's ConditionPathNotEmpty
# enforces the same check at unit-start time, but an in-script check ensures the watcher
# also fails fast when launched manually without the systemd precondition.
ACTIVE_ENV_FILE="$REPO_ROOT/compose/active-env"
if [[ ! -f "$ACTIVE_ENV_FILE" || ! -s "$ACTIVE_ENV_FILE" ]]; then
  echo "[dev-stack-watch] FATAL: $ACTIVE_ENV_FILE is missing or empty — refusing to start" >&2
  echo "[dev-stack-watch]   Create it before starting. Example: echo tailscale > compose/active-env" >&2
  exit 1
fi

POLL_INTERVAL="${POLL_INTERVAL:-60}"
REMOTE="${REMOTE:-origin}"
BRANCH="${BRANCH:-main}"

REBUILD_SCRIPT="$SCRIPT_DIR/dev-stack-auto-rebuild.sh"

# Resolve COMPOSE_CMD with the same default as dev-stack-auto-rebuild.sh uses.
# This is needed to inspect running container counts for cold-start detection.
COMPOSE_CMD="${COMPOSE_CMD:-docker compose -f $REPO_ROOT/compose/dev.yml}"

# Runs the rebuild script with all output tee'd to a timestamped log file so
# failures are visible even when the rebuild script exits before its first echo.
# Propagates the rebuild script's exit code to the caller.
run_rebuild() {
  local log="$STATE_DIR/rebuild-$(date +%Y%m%dT%H%M%S)-$$.log"
  local rc
  set +e
  "$REBUILD_SCRIPT" "$@" 2>&1 | tee "$log"
  rc=${PIPESTATUS[0]}
  set -e
  if [[ $rc -ne 0 ]]; then
    echo "[dev-stack-watch] rebuild exited $rc — log: $log" >&2
  fi
  return $rc
}

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

# Cold-start on startup: if the stack is fully stopped, bring it up immediately without
# waiting for the next commit. Retries with exponential backoff so transient failures
# (e.g. Docker daemon not yet ready) self-heal. Exits non-zero after all attempts so
# systemd Restart=on-failure can take over rather than leaving the stack down indefinitely.
if stack_is_cold; then
  echo "[dev-stack-watch] stack is not running — triggering cold start"
  _cs_attempt=1
  _cs_delay="${COLD_START_RETRY_DELAY:-30}"
  _cs_max="${COLD_START_MAX_ATTEMPTS:-5}"
  while [[ $_cs_attempt -le $_cs_max ]]; do
    if run_rebuild --cold-start "$LAST_KNOWN_SHA" "$LAST_KNOWN_SHA"; then
      echo "[dev-stack-watch] cold start succeeded on attempt $_cs_attempt"
      break
    fi
    echo "[dev-stack-watch] cold start failed (attempt $_cs_attempt/$_cs_max) — see rebuild logs" >&2
    if [[ $_cs_attempt -lt $_cs_max ]]; then
      echo "[dev-stack-watch] retrying in ${_cs_delay}s" >&2
      sleep "$_cs_delay"
      _cs_delay=$(( _cs_delay * 2 ))
    fi
    _cs_attempt=$(( _cs_attempt + 1 ))
  done
  if [[ $_cs_attempt -gt $_cs_max ]]; then
    echo "[dev-stack-watch] cold start failed after $_cs_max attempts — exiting for systemd restart" >&2
    exit 1
  fi
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

  # When on a feature branch the fast-forward above is skipped, leaving the
  # working tree at the branch HEAD. Docker build context comes from the working
  # tree, so stale files cause Docker to hit cached layers for code that changed
  # on main. Fix this by temporarily checking out changed files from origin/main
  # before building, then restoring them afterwards so the branch stays clean.
  _WK_CHECKED_OUT=()
  if [[ "$CURRENT_BRANCH" != "$BRANCH" ]]; then
    mapfile -t _WK_DELTA < <(git diff --name-only "$PREV_SHA" "$NEW_SHA" 2>/dev/null || true)
    if [[ ${#_WK_DELTA[@]} -gt 0 ]]; then
      echo "[dev-stack-watch] on branch $CURRENT_BRANCH — syncing ${#_WK_DELTA[@]} changed file(s) from $REMOTE/$BRANCH into working tree"
      for _f in "${_WK_DELTA[@]}"; do
        if git checkout "$REMOTE/$BRANCH" -- "$_f" 2>/dev/null; then
          _WK_CHECKED_OUT+=("$_f")
        fi
      done
    fi
  fi

  # If the stack is fully stopped, force a cold start so infra services come up too.
  if stack_is_cold; then
    echo "[dev-stack-watch] stack is not running — triggering cold start: $PREV_SHA → $NEW_SHA"
    run_rebuild --cold-start "$PREV_SHA" "$NEW_SHA" || true
  else
    echo "[dev-stack-watch] triggering rebuild: $PREV_SHA → $NEW_SHA"
    # Never let a rebuild failure crash the watch loop.
    run_rebuild "$PREV_SHA" "$NEW_SHA" || true
  fi

  # Restore any files temporarily checked out from main so the working tree
  # stays consistent with the current feature branch.
  if [[ ${#_WK_CHECKED_OUT[@]} -gt 0 ]]; then
    echo "[dev-stack-watch] restoring ${#_WK_CHECKED_OUT[@]} file(s) to $CURRENT_BRANCH"
    for _f in "${_WK_CHECKED_OUT[@]}"; do
      git checkout HEAD -- "$_f" 2>/dev/null || git rm --cached "$_f" 2>/dev/null || true
    done
  fi
done
