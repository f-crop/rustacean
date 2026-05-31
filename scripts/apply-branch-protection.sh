#!/usr/bin/env bash
# apply-branch-protection.sh — declarative bind for the 12 required status checks.
#
# ADR-012 §S5.3: this script is the authoritative source for the FULL branch
# protection payload for main.  GitHub's PUT endpoint is replace-semantics —
# every field in the payload overwrites the live policy, so the defaults here
# must match the intended production policy exactly.
#
# Canonical non-check settings (do not weaken without board approval):
#   enforce_admins:                  true   (admins cannot bypass required checks)
#   required_approving_review_count: 1      (Gate-3 reviewer approval required)
#   dismiss_stale_reviews:           true   (stale approvals cleared on new push)
#
# Usage:
#   scripts/apply-branch-protection.sh            # Apply to origin repo
#   scripts/apply-branch-protection.sh --dry-run  # Print JSON, do not apply
#   scripts/apply-branch-protection.sh --list     # Print one check per line
#   make apply-branch-protection
#
# Requirements: gh CLI authenticated with admin:write on the repository.

set -euo pipefail

REPO="${GH_REPO:-$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null || echo "f-crop/rustacean")}"

# ── 12 required status checks (ADR-012 §S5.2) ────────────────────────────────
# Format: the exact job `name:` string emitted by each workflow.
# Cross-reference: .github/workflows/ci.yml (checks 1-7),
#   pr-hygiene.yml (8), pr-bundle-check.yml (9), pr-migration-hygiene.yml (10),
#   pipeline-e2e.yml (11), runtime-smoke.yml (12).
REQUIRED_CHECKS=(
  "build"
  "test"
  "clippy"
  "fmt"
  "deny"
  "frontend-build"
  "frontend-test"
  "pr-hygiene"
  "pr-bundle-check"
  "pr-migration-hygiene"
  "pipeline-e2e"
  "runtime-smoke"
)

if [[ "${1:-}" == "--list" ]]; then
  printf '%s\n' "${REQUIRED_CHECKS[@]}"
  exit 0
fi

# Build the JSON payload for the branch protection PUT.
# GitHub PUT is replace-semantics: every field here replaces the live value.
# Keep all three security settings in sync with the canonical values above.
CHECKS_JSON=$(printf '%s\n' "${REQUIRED_CHECKS[@]}" \
  | jq -R . | jq -sc '.')

PAYLOAD=$(jq -n \
  --argjson checks "$CHECKS_JSON" \
  '{
    required_status_checks: {
      strict: false,
      contexts: $checks
    },
    enforce_admins: true,
    required_pull_request_reviews: {
      dismiss_stale_reviews: true,
      require_code_owner_reviews: false,
      required_approving_review_count: 1
    },
    restrictions: null,
    allow_force_pushes: false,
    allow_deletions: false,
    block_creations: false,
    required_conversation_resolution: false,
    lock_branch: false,
    allow_fork_syncing: false
  }')

# ── Pre-flight assertions: verify the three security-critical values ──────────
# Catches regressions if the jq template is edited accidentally.
_assert_payload() {
  local field="$1" expected="$2"
  local actual
  actual=$(echo "$PAYLOAD" | jq -r "$field")
  if [[ "$actual" != "$expected" ]]; then
    echo "::error::PAYLOAD assertion failed: $field = $actual (expected $expected)" >&2
    exit 1
  fi
}

_assert_payload '.enforce_admins'                                          'true'
_assert_payload '.required_pull_request_reviews.dismiss_stale_reviews'    'true'
_assert_payload '.required_pull_request_reviews.required_approving_review_count' '1'

if [[ "${1:-}" == "--dry-run" ]]; then
  echo "==> DRY RUN — would PUT the following payload to branches/main/protection:"
  echo "$PAYLOAD" | jq .
  exit 0
fi

echo "==> Applying branch protection for ${REPO} / main"
echo "    Required checks (${#REQUIRED_CHECKS[@]}):"
printf '      - %s\n' "${REQUIRED_CHECKS[@]}"
echo ""

gh api \
  "repos/${REPO}/branches/main/protection" \
  -X PUT \
  --input - <<< "$PAYLOAD"

echo ""
echo "==> Verifying applied rules..."
LIVE=$(gh api "repos/${REPO}/branches/main/protection" \
  --jq '.required_status_checks.contexts | sort | .[]')

MISSING=0
for check in "${REQUIRED_CHECKS[@]}"; do
  if ! echo "$LIVE" | grep -qxF "$check"; then
    echo "  MISSING: $check"
    MISSING=1
  fi
done

if [[ "$MISSING" -eq 1 ]]; then
  echo "::error::Some required checks were not bound. See output above."
  exit 1
fi

echo "==> All ${#REQUIRED_CHECKS[@]} required checks are bound."
