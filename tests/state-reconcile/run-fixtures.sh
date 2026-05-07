#!/usr/bin/env bash
# Fixture tests for manage_drift_issue in scripts/state-reconcile.sh.
#
# Verifies:
#   AC-1  Drift + terminal umbrella (done|cancelled) → new umbrella created
#   AC-4  Drift + active umbrella (todo|in_progress) → comment-only, no new umbrella
#
# The production manage_drift_issue function is extracted from the script via awk
# and eval'd in an isolated subshell with mocked external calls, so we are testing
# the actual production code rather than a hand-rolled copy.
#
# Usage:
#   bash tests/state-reconcile/run-fixtures.sh
#   make state-reconcile-fixtures

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
PRODUCTION_SCRIPT="$REPO_ROOT/scripts/state-reconcile.sh"

FAIL=0
RESULTS=()

_expect_result() {
    local label="$1" expected="$2" actual_exit="$3"
    if [[ "$expected" == "pass" && "$actual_exit" -eq 0 ]]; then
        echo "  OK (exit 0 as expected)"
        RESULTS+=("  PASS  $label")
    elif [[ "$expected" == "fail" && "$actual_exit" -ne 0 ]]; then
        echo "  OK (exit $actual_exit as expected)"
        RESULTS+=("  PASS  $label")
    else
        echo "  WRONG: expected=$expected actual_exit=$actual_exit"
        FAIL=$((FAIL + 1))
        RESULTS+=("  FAIL  $label")
    fi
}

# Extract manage_drift_issue (and its direct callees) from the production script.
# Uses brace-depth tracking; balanced jq JSON blocks do not distort the count.
_extract_fn() {
    local fn_name="$1"
    awk -v name="$fn_name" '
        $0 ~ "^" name "\\(\\)" { capture=1; depth=0 }
        capture {
            print
            n = split($0, ch, "")
            for (i = 1; i <= n; i++) {
                if (ch[i] == "{") depth++
                else if (ch[i] == "}") {
                    depth--
                    if (depth == 0) { capture=0; break }
                }
            }
        }
    ' "$PRODUCTION_SCRIPT"
}

# Run manage_drift_issue in a fully isolated subshell.
#
# Args:
#   $1  umbrella_status — "none" | "done" | "cancelled" | "todo" | "in_progress"
#   $2  drift_line      — non-empty string = drift present; empty = no drift
#
# Stdout: one "METHOD URL" line per curl call made during the run.
_run_fixture() {
    local umbrella_status="$1"
    local drift_input="${2:-}"

    local tmpdir
    tmpdir=$(mktemp -d)
    # shellcheck disable=SC2064
    trap "rm -rf $tmpdir" RETURN

    local curl_log="$tmpdir/curl.log"
    touch "$curl_log"

    # Extract production functions once; export to subshell via env
    local fn_manage fn_find
    fn_manage=$(_extract_fn manage_drift_issue)
    fn_find=$(_extract_fn find_drift_umbrella_issue)

    # Run in a subshell — all external calls are mocked
    (
        export PAPERCLIP_API_KEY="test-key"
        export API="http://mock.test"
        export COMPANY="cmp-001"
        export AGENT="agt-001"
        export RUN_ID="run-001"
        export RECONCILE_BASELINE_DATE="2026-01-01T00:00:00Z"
        export CURL_LOG="$curl_log"
        export MOCK_STATUS="$umbrella_status"

        log()  { :; }
        warn() { :; }

        # Record METHOD+URL for each curl call; return minimal valid JSON
        curl() {
            local method="GET" url="" prev=""
            for arg in "$@"; do
                [[ "$prev" == "-X" ]] && method="$arg"
                [[ "$arg" == http* ]] && url="$arg"
                prev="$arg"
            done
            printf '%s %s\n' "$method" "$url" >> "$CURL_LOG"

            if [[ "$url" =~ /companies/[^/]+/issues$ && "$method" == "POST" ]]; then
                printf '{"id":"new-id-888","identifier":"RUSAA-888"}\n'
            elif [[ "$url" =~ /issues/[^/]+/comments$ && "$method" == "POST" ]]; then
                printf '{}\n'
            elif [[ "$method" == "PATCH" ]]; then
                printf '{}\n'
            fi
        }

        # Override find_drift_umbrella_issue with a synthetic stub;
        # do NOT eval the production version to keep the test focused on
        # manage_drift_issue's decision logic.
        find_drift_umbrella_issue() {
            if [[ "$MOCK_STATUS" == "none" ]]; then
                echo ""
            else
                echo "existing-id $MOCK_STATUS RUSAA-777"
            fi
        }

        # handle_closed_issue_drift_policy is about dim-(d) closed issues,
        # not the umbrella itself — mock it to isolate manage_drift_issue.
        handle_closed_issue_drift_policy() { :; }

        # Eval the production manage_drift_issue function
        eval "$fn_manage"

        if [[ -n "$drift_input" ]]; then
            manage_drift_issue "dev" "$drift_input"
        else
            manage_drift_issue "dev"
        fi
    ) 2>/dev/null

    cat "$curl_log"
}

# ── AC-1: done umbrella + drift → NEW umbrella created ───────────────────────

echo ""
echo "==> fixture: AC-1 / done umbrella: drift detected → new umbrella created  [expect: pass]"
curl_calls=$(_run_fixture "done" "[a] control-api: container not running")
created=$(echo "$curl_calls" | grep -cE "^POST http://mock\.test/api/companies/cmp-001/issues$" || true)
commented=$(echo "$curl_calls" | grep -cE "^POST http://mock\.test/api/issues/existing-id/comments$" || true)
[[ "$created" -ge 1 && "$commented" -eq 0 ]]
_expect_result "AC-1 done: new umbrella created, no comment on closed issue" "pass" "$?"

# ── AC-1: cancelled umbrella + drift → NEW umbrella created ──────────────────

echo ""
echo "==> fixture: AC-1 / cancelled umbrella: drift detected → new umbrella created  [expect: pass]"
curl_calls=$(_run_fixture "cancelled" "[b] control-api: required key GRAPH_DB_URL missing")
created=$(echo "$curl_calls" | grep -cE "^POST http://mock\.test/api/companies/cmp-001/issues$" || true)
commented=$(echo "$curl_calls" | grep -cE "^POST http://mock\.test/api/issues/existing-id/comments$" || true)
[[ "$created" -ge 1 && "$commented" -eq 0 ]]
_expect_result "AC-1 cancelled: new umbrella created, no comment on cancelled issue" "pass" "$?"

# ── AC-1: no umbrella at all + drift → NEW umbrella created ──────────────────

echo ""
echo "==> fixture: AC-1 / no umbrella: drift detected → new umbrella created  [expect: pass]"
curl_calls=$(_run_fixture "none" "[c] control: 2 unapplied migration(s)")
created=$(echo "$curl_calls" | grep -cE "^POST http://mock\.test/api/companies/cmp-001/issues$" || true)
[[ "$created" -ge 1 ]]
_expect_result "AC-1 no-umbrella: new umbrella created" "pass" "$?"

# ── AC-4: in_progress umbrella + drift → comment only ────────────────────────

echo ""
echo "==> fixture: AC-4 / in_progress umbrella: drift detected → comment only  [expect: pass]"
curl_calls=$(_run_fixture "in_progress" "[a] embed-worker: container not running")
created=$(echo "$curl_calls" | grep -cE "^POST http://mock\.test/api/companies/cmp-001/issues$" || true)
commented=$(echo "$curl_calls" | grep -cE "^POST http://mock\.test/api/issues/existing-id/comments$" || true)
[[ "$created" -eq 0 && "$commented" -ge 1 ]]
_expect_result "AC-4 in_progress: comment-only, no new umbrella" "pass" "$?"

# ── AC-4: todo umbrella + drift → comment only ───────────────────────────────

echo ""
echo "==> fixture: AC-4 / todo umbrella: drift detected → comment only  [expect: pass]"
curl_calls=$(_run_fixture "todo" "[d] RUSAA-500: fingerprint SHA=abc123, current main=def456")
created=$(echo "$curl_calls" | grep -cE "^POST http://mock\.test/api/companies/cmp-001/issues$" || true)
commented=$(echo "$curl_calls" | grep -cE "^POST http://mock\.test/api/issues/existing-id/comments$" || true)
[[ "$created" -eq 0 && "$commented" -ge 1 ]]
_expect_result "AC-4 todo: comment-only, no new umbrella" "pass" "$?"

# ── Regression: no drift + active umbrella → umbrella closed ─────────────────

echo ""
echo "==> fixture: regression / no-drift with active umbrella → umbrella closed  [expect: pass]"
curl_calls=$(_run_fixture "in_progress" "")
closed=$(echo "$curl_calls" | grep -cE "^PATCH http://mock\.test/api/issues/existing-id$" || true)
[[ "$closed" -ge 1 ]]
_expect_result "regression: no-drift closes active umbrella" "pass" "$?"

# ── Regression: no drift + no umbrella → no API calls ────────────────────────

echo ""
echo "==> fixture: regression / no-drift no-umbrella → no API calls  [expect: pass]"
curl_calls=$(_run_fixture "none" "")
call_count=$(echo "$curl_calls" | grep -c . || true)
[[ "$call_count" -eq 0 ]]
_expect_result "regression: no-drift no-umbrella is a no-op" "pass" "$?"

# ── Summary ───────────────────────────────────────────────────────────────────

echo ""
echo "==========================================="
echo "state-reconcile fixture summary:"
for r in "${RESULTS[@]}"; do
    echo "$r"
done
echo "==========================================="

if [[ "$FAIL" -eq 0 ]]; then
    echo "ALL FIXTURES PASSED"
else
    echo "$FAIL fixture(s) had wrong outcome — routine logic is broken"
fi

exit "$FAIL"
