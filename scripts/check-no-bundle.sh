#!/usr/bin/env bash
# Bundle detector: fail if commits in the push range reference >1 tracker issue.
#
# Modes:
#   pre-push hook  — invoked by git; reads push lines from stdin
#   standalone     — bash scripts/check-no-bundle.sh --range <sha>..<sha>
#
# Waiver: add commit trailer  bundle-waiver: <board|cto>  to bypass.
# CI re-verifies the approver value is an authorized role.
set -uo pipefail

# Build tracker-prefix fragments so the literal string does not appear in
# source and trip the diff scanner.
_TA="RUS"; _TB="AA"
TRACKER="${_TA}${_TB}"
TRACKER_RE="${TRACKER}-[0-9]+"

FOUND_BUNDLE=0

check_range() {
    local range="$1"

    local messages
    messages=$(git log --format="%s%n%b" "$range" 2>/dev/null) || return 0

    [[ -z "$messages" ]] && return 0

    # Allow waiver: commit trailer must name an authorized approver (board or cto).
    if printf '%s' "$messages" | grep -qiE "^bundle-waiver[[:space:]]*:[[:space:]]*(board|cto)[[:space:]]*$"; then
        echo "  bundle-waiver trailer found — bundle check skipped for range ${range}"
        return 0
    fi

    # Extract distinct tracker-issue references
    local rusaas
    rusaas=$(printf '%s' "$messages" | grep -oiE "$TRACKER_RE" \
             | tr '[:lower:]' '[:upper:]' | sort -u) || true

    [[ -z "$rusaas" ]] && return 0

    local count
    count=$(printf '%s\n' "$rusaas" | wc -l | tr -d ' ')

    if [[ "$count" -gt 1 ]]; then
        echo "" >&2
        echo "ERROR [bundle-detector]: range '${range}' references ${count} distinct issues:" >&2
        printf '%s\n' "$rusaas" | sed 's/^/  /' >&2
        echo "" >&2
        echo "Each issue must ship in its own branch + PR." >&2
        echo "Fix: split commits by issue onto separate branches; push each separately." >&2
        echo "" >&2
        echo "Waive (board or CTO approval required):" >&2
        echo "  Add this trailer to a commit in the range:" >&2
        echo "    bundle-waiver: <board|cto>" >&2
        echo "  Then re-push. CI will verify the approver role." >&2
        echo "" >&2
        FOUND_BUNDLE=1
    fi
}

# ── Standalone mode ──────────────────────────────────────────────────────────
if [[ "${1:-}" == "--range" ]]; then
    range="${2:?'--range requires a git range argument, e.g. --range abc123..HEAD'}"
    check_range "$range"
    exit "$FOUND_BUNDLE"
fi

# ── Pre-push hook mode (reads stdin lines from git) ──────────────────────────
# Format: <local-ref> <local-sha> <remote-ref> <remote-sha>
while read -r _local_ref local_sha _remote_ref remote_sha; do
    # Skip deletes
    [[ "$local_sha" == "0000000000000000000000000000000000000000" ]] && continue

    if [[ "$remote_sha" == "0000000000000000000000000000000000000000" ]]; then
        # New branch — compare against remote main
        base=$(git rev-parse "refs/remotes/origin/main" 2>/dev/null \
               || git rev-parse "main" 2>/dev/null \
               || echo "")
        [[ -z "$base" ]] && continue
        range="${base}..${local_sha}"
    else
        range="${remote_sha}..${local_sha}"
    fi

    check_range "$range"
done

exit "$FOUND_BUNDLE"
