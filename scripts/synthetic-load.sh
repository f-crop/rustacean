#!/usr/bin/env bash
# scripts/synthetic-load.sh — synthetic-load harness entrypoint for Wave 8 S7.
#
# Usage:
#   scripts/synthetic-load.sh start      # start the 7-day soak (default)
#   scripts/synthetic-load.sh provision  # pre-provision the tenant pool and exit
#   scripts/synthetic-load.sh report     # print the latest daily summary and exit
#   scripts/synthetic-load.sh status     # tail the harness-state.json
#
# Required env vars:
#   SYNTHETIC_LOAD_TARGET          — control-api base URL (e.g. https://pre-prod.example.com)
#   SYNTHETIC_LOAD_ADMIN_TOKEN     — same as RB_ADMIN_TOKEN on the target stack
#   SYNTHETIC_LOAD_DATABASE_URL    — Postgres DSN for email verification (same as DATABASE_URL)
#
# Optional env vars (see services/synthetic-load/src/config.rs for full list):
#   SYNTHETIC_LOAD_TENANT_COUNT       (default: 10)
#   SYNTHETIC_LOAD_DAYS               (default: 7)
#   SYNTHETIC_LOAD_PROMETHEUS_URL     (default: unset)
#   SYNTHETIC_LOAD_SERVICE_URLS       (default: SYNTHETIC_LOAD_TARGET)
#   SYNTHETIC_LOAD_STATE_DIR          (default: ~/.local/state/rustbrain/synthetic-load)
#
# Prerequisites:
#   • The binary must be built: cargo build --release -p synthetic-load
#   • Or run via Docker: docker run --env-file ... ghcr.io/f-crop/rustacean/synthetic-load start

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

COMMAND="${1:-start}"

BINARY="${SYNTHETIC_LOAD_BINARY:-${REPO_ROOT}/target/release/synthetic-load}"

if [[ ! -x "${BINARY}" ]]; then
    echo "synthetic-load binary not found at ${BINARY}" >&2
    echo "Build it first: cargo build --release -p synthetic-load" >&2
    exit 1
fi

# Resolve state dir for status command.
STATE_DIR="${SYNTHETIC_LOAD_STATE_DIR:-${HOME}/.local/state/rustbrain/synthetic-load}"

case "${COMMAND}" in
    start)
        echo "==> synthetic-load: starting 7-day soak"
        echo "    target   : ${SYNTHETIC_LOAD_TARGET:-<unset>}"
        echo "    state_dir: ${STATE_DIR}"
        exec "${BINARY}" start
        ;;
    provision)
        echo "==> synthetic-load: provisioning tenant pool"
        exec "${BINARY}" provision
        ;;
    report)
        exec "${BINARY}" report
        ;;
    status)
        state_file="${STATE_DIR}/harness-state.json"
        if [[ -f "${state_file}" ]]; then
            python3 -m json.tool "${state_file}" 2>/dev/null || cat "${state_file}"
        else
            echo "no harness state found at ${state_file}" >&2
            exit 1
        fi
        ;;
    build-info)
        exec "${BINARY}" build-info
        ;;
    *)
        echo "unknown command: ${COMMAND}" >&2
        echo "usage: $0 {start|provision|report|status|build-info}" >&2
        exit 1
        ;;
esac
