#!/usr/bin/env bash
# smoke-cargo-rss.sh — Validate that cargo invocations stay within the RSS budget.
#
# Acceptance criteria:
#   - No single rust-lld/mold process exceeds 2 GB RSS
#   - Peak combined RSS across all concurrent cargo invocations stays under 4 GB
#   - No more than one cargo build/test runs concurrently (enforced by cargo-locked)
#
# Run from the workspace root:
#   bash scripts/smoke-cargo-rss.sh
#
# Requires: cargo-locked in PATH, /usr/bin/time (GNU time for --format)

set -euo pipefail

WORKSPACE_ROOT="$(git rev-parse --show-toplevel)"
RESULTS_DIR="/tmp/rusaa-rss-smoke-$$"
mkdir -p "$RESULTS_DIR"

LOCK_FILE="${RUSAA_CARGO_LOCK_FILE:-/tmp/rusaa-cargo.lock}"
PEAK_RSS_MB_LIMIT=2048     # per-linker RSS cap
COMBINED_RSS_MB_LIMIT=4096 # combined RSS cap across all concurrent invocations

echo "=== Cargo RSS Smoke Test ==="
echo "Workspace: $WORKSPACE_ROOT"
echo "Lock file: $LOCK_FILE"
echo ""

# ── Helper: measure peak RSS of a command ─────────────────────────────────────
measure_rss() {
    local label="$1"
    shift
    local out="$RESULTS_DIR/$label.txt"
    # GNU time reports max RSS in KB
    /usr/bin/time -f "%M" -o "$out" "$@" 2>/dev/null
    local rss_kb
    rss_kb=$(cat "$out")
    echo "$((rss_kb / 1024))"  # MB
}

# ── 1. Serial check: single cargo-locked invocation stays under 2 GB ──────────
echo "--- Test 1: Single cargo-locked check (expect < ${PEAK_RSS_MB_LIMIT} MB peak) ---"
cd "$WORKSPACE_ROOT"
rss_mb=$(measure_rss "single" cargo-locked check -p rb-tracing 2>/dev/null)
echo "Peak RSS: ${rss_mb} MB"
if (( rss_mb > PEAK_RSS_MB_LIMIT )); then
    echo "FAIL: ${rss_mb} MB exceeds ${PEAK_RSS_MB_LIMIT} MB cap"
    exit 1
else
    echo "PASS"
fi
echo ""

# ── 2. Parallel probe: 3 concurrent cargo-locked invocations serialize ─────────
echo "--- Test 2: 3 parallel cargo-locked invocations (flock ensures serial execution) ---"

PIDS=()
LABELS=(alpha beta gamma)
CRATES=(rb-tracing rb-kafka rb-tenant)

for i in 0 1 2; do
    label="${LABELS[$i]}"
    crate="${CRATES[$i]}"
    (
        # Each agent sets jobs=1 via user-level ~/.cargo/config.toml (setup-mold.sh);
        # cargo-locked additionally serializes cross-agent via flock.
        rss=$(measure_rss "$label" cargo-locked check -p "$crate" 2>/dev/null)
        echo "$rss" > "$RESULTS_DIR/${label}.rss"
    ) &
    PIDS+=($!)
done

# Wait for all three
for pid in "${PIDS[@]}"; do
    wait "$pid"
done

# Collect results
total_rss=0
all_pass=true
for label in "${LABELS[@]}"; do
    rss_file="$RESULTS_DIR/${label}.rss"
    if [[ -f "$rss_file" ]]; then
        rss=$(cat "$rss_file")
        echo "  Agent $label peak RSS: ${rss} MB"
        total_rss=$((total_rss + rss))
        if (( rss > PEAK_RSS_MB_LIMIT )); then
            echo "  FAIL: ${rss} MB exceeds ${PEAK_RSS_MB_LIMIT} MB per-agent cap"
            all_pass=false
        fi
    else
        echo "  WARN: No RSS result for $label"
        all_pass=false
    fi
done

echo "Combined RSS across 3 agents: ${total_rss} MB"
if (( total_rss > COMBINED_RSS_MB_LIMIT )); then
    echo "FAIL: Combined ${total_rss} MB exceeds ${COMBINED_RSS_MB_LIMIT} MB company-wide cap"
    all_pass=false
fi

if $all_pass; then
    echo "PASS: all 3 agents within budget"
else
    exit 1
fi

echo ""
echo "=== Smoke test PASS ==="
rm -rf "$RESULTS_DIR"
