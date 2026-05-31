#!/usr/bin/env bash
# grafana-lint.sh — ADR-012 §S3 binding between S2 metric registry and
# S3 Grafana dashboards.
#
# For every *.json file in the dashboard directory, extract all rb_*
# metric names that appear in PromQL "expr" fields and verify each one
# is listed in the S2 frozen registry.  Exits 1 if any unknown metric
# is found; exits 0 when all dashboards pass.
#
# Usage (from repo root):
#   bash scripts/grafana-lint.sh
#   bash scripts/grafana-lint.sh [dashboard-dir] [registry-file]
set -euo pipefail

DASHBOARD_DIR="${1:-infra/grafana/dashboards}"
REGISTRY_FILE="${2:-infra/grafana/metrics-registry.txt}"

# ── Prerequisite checks ──────────────────────────────────────────────────────

if ! command -v jq &>/dev/null; then
  echo "ERROR: jq is required but not installed" >&2
  exit 1
fi

if [[ ! -f "$REGISTRY_FILE" ]]; then
  echo "ERROR: metrics registry not found: $REGISTRY_FILE" >&2
  exit 1
fi

if [[ ! -d "$DASHBOARD_DIR" ]]; then
  echo "ERROR: dashboard directory not found: $DASHBOARD_DIR" >&2
  exit 1
fi

# ── Load registry ────────────────────────────────────────────────────────────

declare -A REGISTRY
while IFS= read -r line; do
  [[ -z "$line" || "$line" == \#* ]] && continue
  REGISTRY["$line"]=1
done < "$REGISTRY_FILE"

registry_size="${#REGISTRY[@]}"
echo "grafana-lint: loaded ${registry_size} metrics from ${REGISTRY_FILE}"

# ── Check each dashboard ─────────────────────────────────────────────────────

failures=0
dashboards=0

for dashboard_file in "${DASHBOARD_DIR}"/*.json; do
  [[ -f "$dashboard_file" ]] || continue
  dashboards=$((dashboards + 1))
  dashboard_name=$(basename "$dashboard_file")

  # Extract all PromQL expr strings via jq recursive descent, then
  # grep for rb_<name> tokens.  Normalize Prometheus histogram suffixes
  # (_bucket, _sum, _count, _created) to their base metric name so the
  # registry only stores one entry per histogram.
  dashboard_metrics=$(
    jq -r '.. | .expr? | strings' "$dashboard_file" 2>/dev/null \
      | grep -oP 'rb_[a-z][a-z0-9_]*' \
      | sed -E 's/_(bucket|sum|count|created)$//' \
      | sort -u \
      || true
  )

  panel_failures=0
  while IFS= read -r metric_name; do
    [[ -z "$metric_name" ]] && continue
    if [[ -z "${REGISTRY[$metric_name]+_}" ]]; then
      echo "  FAIL [$dashboard_name]: '$metric_name' is not in the S2 registry" >&2
      panel_failures=$((panel_failures + 1))
      failures=$((failures + 1))
    fi
  done <<< "$dashboard_metrics"

  if [[ $panel_failures -eq 0 ]]; then
    echo "  OK   [$dashboard_name]"
  fi
done

echo ""
echo "grafana-lint: checked ${dashboards} dashboard(s)"

if [[ $failures -gt 0 ]]; then
  echo "grafana-lint: FAIL — ${failures} unknown metric reference(s) across dashboards" >&2
  exit 1
fi

echo "grafana-lint: PASS — all panel metrics are in the S2 registry"
