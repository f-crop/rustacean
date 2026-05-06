#!/usr/bin/env bash
# dev-stack-up.sh — bring up the dev stack with env validation
#
# Usage:
#   scripts/dev-stack-up.sh --env-file compose/<env>  [-- <extra compose args>]
#   scripts/dev-stack-up.sh --env-file compose/tailscale.env -- -d
#
# What this script does:
#   1. Requires --env-file (refuses to start without one).
#   2. Sources the env file into the shell (fix for docker --env-file not exporting
#      vars to the wrapping shell — see feedback_envfile_not_exported_to_shell.md).
#   3. Runs compose/scripts/validate-env.sh against the env file to catch missing
#      required vars and format violations before any container starts.
#   4. Starts one canary container (alpine), dumps its env, and validates that
#      service-critical vars are present inside Docker (catches the "started without
#      --env-file" class where host env ≠ container env).
#   5. Brings up the full stack.
#
# Environment variables:
#   RB_REPO_PATH        Repo root (default: parent of this script)
#   SKIP_CANARY         Set to 1 to skip the canary container check
#
# This script is the safe replacement for bare `docker compose up -d`.
# The auto-rebuild script (dev-stack-auto-rebuild.sh) handles incremental rebuilds
# and does NOT need to call this — it runs its own health checks.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${RB_REPO_PATH:-"$(cd "$SCRIPT_DIR/.." && pwd)"}"
SCHEMA_FILE="$REPO_ROOT/compose/env.schema.toml"
VALIDATOR="$REPO_ROOT/compose/scripts/validate-env.sh"

# -- Args --------------------------------------------------------------------

ENV_FILE=""
EXTRA_ARGS=()
IN_EXTRA=false

usage() {
  echo "Usage: $0 --env-file <env-file> [-- <extra docker compose args>]" >&2
  echo "" >&2
  echo "Examples:" >&2
  echo "  $0 --env-file compose/dev.env -- -d" >&2
  echo "  $0 --env-file compose/tailscale.env -- -d" >&2
  exit 2
}

while [[ $# -gt 0 ]]; do
  if $IN_EXTRA; then
    EXTRA_ARGS+=("$1")
    shift
    continue
  fi
  case "$1" in
    --env-file) ENV_FILE="$2"; shift 2 ;;
    --) IN_EXTRA=true; shift ;;
    -h|--help) usage ;;
    *) EXTRA_ARGS+=("$1"); shift ;;
  esac
done

if [[ -z "$ENV_FILE" ]]; then
  echo "" >&2
  echo "ERROR: --env-file is required." >&2
  echo "" >&2
  echo "Refusing to start without an explicit env file. Starting the stack without" >&2
  echo "one causes docker compose to fall back to hardcoded defaults in dev.yml," >&2
  echo "which silently sets wrong values (e.g. RB_BASE_URL=http://localhost:8080" >&2
  echo "instead of the frontend origin). This class of misconfiguration caused" >&2
  echo "multiple Wave 6 env-drift incidents." >&2
  echo "" >&2
  echo "Create your env file from the template:" >&2
  echo "  cp compose/dev.env.tpl compose/dev.env  # then edit for your environment" >&2
  echo "" >&2
  usage
fi

if [[ ! -f "$ENV_FILE" ]]; then
  echo "ERROR: env file not found: $ENV_FILE" >&2
  echo "" >&2
  echo "Create it from the template:" >&2
  echo "  cp compose/dev.env.tpl compose/dev.env" >&2
  exit 2
fi

echo "[dev-stack-up] ENV_FILE=$ENV_FILE"

# -- Schema validation --------------------------------------------------------
# Validate BEFORE sourcing so malformed values don't corrupt the shell.

if [[ ! -f "$VALIDATOR" ]]; then
  echo "[dev-stack-up] WARNING: validator not found at $VALIDATOR — skipping schema check" >&2
else
  echo "[dev-stack-up] validating env file against schema..."
  if ! bash "$VALIDATOR" "$ENV_FILE"; then
    echo "" >&2
    echo "[dev-stack-up] ABORTED: env file failed schema validation." >&2
    echo "Fix the errors above, then re-run this script." >&2
    exit 1
  fi
fi

# -- Source env file into shell -----------------------------------------------
# docker compose --env-file only passes vars to containers, NOT to this shell.
# We need them in the shell too for health-check port lookups.
# Parse safely: read KEY=VALUE pairs and export each one to avoid shell-splitting
# on unquoted values. Skips blank lines and comments.

while IFS= read -r raw_line; do
  # strip leading/trailing whitespace and skip comments/blanks
  line="${raw_line#"${raw_line%%[![:space:]]*}"}"
  [[ -z "$line" || "$line" == \#* ]] && continue
  [[ "$line" != *=* ]] && continue
  key="${line%%=*}"
  val="${line#*=}"
  # Only export valid identifier keys
  if [[ "$key" =~ ^[a-zA-Z_][a-zA-Z0-9_]*$ ]]; then
    export "$key=$val"
  fi
done < "$ENV_FILE"

# -- Canary container check ---------------------------------------------------
# Start a minimal container with the same env-file to verify that service-critical
# vars arrive inside Docker. This catches the case where the env file is malformed
# in a way that docker compose silently ignores (e.g. Windows line endings, BOM).

CANARY_VARS=(
  RB_BASE_URL
  RB_DATABASE_URL
  DATABASE_URL
  NEO4J_PASSWORD
)

if [[ "${SKIP_CANARY:-0}" != "1" ]] && command -v docker &>/dev/null; then
  echo "[dev-stack-up] running canary container to verify env vars reach Docker..."

  # Determine compose files from env file name
  COMPOSE_FILE_ARGS="-f $REPO_ROOT/compose/dev.yml"
  BASENAME="$(basename "$ENV_FILE" .env)"
  OVERLAY="$REPO_ROOT/compose/${BASENAME}.yml"
  if [[ -f "$OVERLAY" && "$OVERLAY" != "$REPO_ROOT/compose/dev.yml" ]]; then
    COMPOSE_FILE_ARGS="$COMPOSE_FILE_ARGS -f $OVERLAY"
  fi

  CANARY_ERRORS=0
  for VAR in "${CANARY_VARS[@]}"; do
    # Only check vars that exist in the env file (schema handles "required" check)
    HOST_VAL="${!VAR:-}"
    [[ -z "$HOST_VAL" ]] && continue

    # Check what value the container would see using env-file interpolation
    CONTAINER_VAL="$(
      docker compose --env-file "$ENV_FILE" $COMPOSE_FILE_ARGS \
        run --rm --no-deps --entrypoint sh alpine -c "echo \$$VAR" 2>/dev/null \
      || true
    )"

    if [[ "$CONTAINER_VAL" != "$HOST_VAL" && -n "$HOST_VAL" ]]; then
      echo "[dev-stack-up] CANARY MISMATCH: $VAR" >&2
      echo "  host sees:      $HOST_VAL" >&2
      echo "  container sees: ${CONTAINER_VAL:-(empty)}" >&2
      CANARY_ERRORS=$((CANARY_ERRORS + 1))
    fi
  done

  if [[ $CANARY_ERRORS -gt 0 ]]; then
    echo "" >&2
    echo "[dev-stack-up] ABORTED: $CANARY_ERRORS canary mismatch(es) detected." >&2
    echo "The env-file may have encoding issues or incorrect syntax." >&2
    exit 1
  fi
  echo "[dev-stack-up] canary check passed — env vars reach containers correctly"
else
  echo "[dev-stack-up] skipping canary container check (SKIP_CANARY=1 or docker not available)"
fi

# -- Bring up the stack -------------------------------------------------------

COMPOSE_BASE="-f $REPO_ROOT/compose/dev.yml"
BASENAME="$(basename "$ENV_FILE" .env)"
OVERLAY="$REPO_ROOT/compose/${BASENAME}.yml"
if [[ -f "$OVERLAY" && "$OVERLAY" != "$REPO_ROOT/compose/dev.yml" ]]; then
  COMPOSE_BASE="$COMPOSE_BASE -f $OVERLAY"
fi

COMPOSE_CMD="docker compose --env-file $ENV_FILE $COMPOSE_BASE"

echo "[dev-stack-up] running: $COMPOSE_CMD up ${EXTRA_ARGS[*]}"
# shellcheck disable=SC2086
exec $COMPOSE_CMD up "${EXTRA_ARGS[@]}"
