#!/usr/bin/env bash
# Unit tests for check-dev-stack-drift.sh exit-code contract.
#
# Injects mock git/docker executables via PATH — no real Docker daemon or git
# state required.  Each test case controls which containers "exist" and what
# SHA label they carry via files in a per-test MOCK_DIR.
#
# Mock git reads $MOCK_DIR/head_sha for rev-parse responses.
# Mock docker reads $MOCK_DIR/sha_<svc_key> (e.g. sha_control_api) for label
# responses; a missing file means the container is absent/not-running.
#
# Exit: 0 all tests passed, 1 one or more failed.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DRIFT_SCRIPT="$SCRIPT_DIR/check-dev-stack-drift.sh"

PASS=0
FAIL=0

# ── Mock binary directory (written once; read $MOCK_DIR at runtime) ─────────

MOCK_BIN_DIR="$(mktemp -d)"
cleanup() { rm -rf "$MOCK_BIN_DIR"; }
trap cleanup EXIT

cat > "$MOCK_BIN_DIR/git" <<'GITMOCK'
#!/usr/bin/env bash
if   [[ "$1" == "fetch" ]];                                   then exit 0; fi
if   [[ "$1 $2" == "rev-parse origin/main" ]];                then cat "$MOCK_DIR/head_sha"; exit 0; fi
if   [[ "$1" == "show" ]];                                    then echo "1000000000"; exit 0; fi
if   [[ "$1 $2 $3" == "merge-base --is-ancestor" ]];          then exit 1; fi
exit 128
GITMOCK
chmod +x "$MOCK_BIN_DIR/git"

cat > "$MOCK_BIN_DIR/docker" <<'DOCKMOCK'
#!/usr/bin/env bash
# Handles: docker inspect --format FORMAT CONTAINER
[[ "$1" == "inspect" ]] || exit 0

format="$3"
container="$4"

# Derive service key: rustbrain-dev-control-api-1 → control_api
svc="${container#rustbrain-dev-}"
svc="${svc%-1}"
svc_key="${svc//-/_}"
sha_file="$MOCK_DIR/sha_${svc_key}"

if [[ "$format" == *State.Running* ]]; then
  [[ -f "$sha_file" ]] && echo "true" && exit 0
  exit 1
fi

if [[ "$format" == *git_sha* ]]; then
  [[ -f "$sha_file" ]] || exit 1
  cat "$sha_file"
  exit 0
fi

exit 1
DOCKMOCK
chmod +x "$MOCK_BIN_DIR/docker"

# ── Helpers ──────────────────────────────────────────────────────────────────

# setup_mock HEAD_SHA [svc=value ...]
#   value="HEAD"    → container is running at HEAD sha (current)
#   value="<sha>"   → container is running at that sha (drifted)
#   value=""        → container is running but has no git_sha label
#   value="absent"  → container is not running (skipped by drift script)
setup_mock() {
  local mock_dir
  mock_dir="$(mktemp -d)"
  local head_sha="$1"; shift
  echo "$head_sha" > "$mock_dir/head_sha"

  for pair in "$@"; do
    local svc="${pair%%=*}"
    local sha="${pair#*=}"
    local key="${svc//-/_}"
    if [[ "$sha" == "absent" ]]; then
      : # no file → container absent
    elif [[ "$sha" == "HEAD" ]]; then
      echo "$head_sha" > "$mock_dir/sha_${key}"
    else
      echo "$sha" > "$mock_dir/sha_${key}"  # includes empty-string case
    fi
  done

  echo "$mock_dir"
}

# assert_exit NAME EXPECTED_EXIT MOCK_DIR
assert_exit() {
  local name="$1" expected="$2" mock_dir="$3"
  local repo_dir
  repo_dir="$(mktemp -d)"

  set +e
  RB_REPO_PATH="$repo_dir" MOCK_DIR="$mock_dir" \
    PATH="$MOCK_BIN_DIR:$PATH" \
    "$DRIFT_SCRIPT" --no-fetch > /dev/null 2>&1
  local actual=$?
  set -e

  rm -rf "$repo_dir" "$mock_dir"

  if [[ "$actual" -eq "$expected" ]]; then
    printf "  PASS  %s (exit %s)\n" "$name" "$actual"
    PASS=$(( PASS + 1 ))
  else
    printf "  FAIL  %s — expected exit %s, got %s\n" "$name" "$expected" "$actual"
    FAIL=$(( FAIL + 1 ))
  fi
}

# ── Test cases ────────────────────────────────────────────────────────────────

HEAD_SHA="aaaa1234bbbb5678cccc9012dddd3456eeee7890aaaa1234"
OLD_SHA="ffff1234aaaa5678bbbb9012cccc3456dddd7890ffff1234"

echo "=== check-dev-stack-drift.sh exit-code tests ==="
echo ""

# Stack fully stopped: all containers absent → nothing drifted → exit 0
assert_exit "stack fully down (all absent)" 0 \
  "$(setup_mock "$HEAD_SHA" control-api=absent agent-runner=absent frontend=absent)"

# All running containers match HEAD → exit 0
assert_exit "all containers current" 0 \
  "$(setup_mock "$HEAD_SHA" control-api=HEAD agent-runner=HEAD frontend=HEAD)"

# Mixed: some current, one behind → exit 1
assert_exit "one container behind HEAD" 1 \
  "$(setup_mock "$HEAD_SHA" control-api=HEAD agent-runner="$OLD_SHA" frontend=HEAD)"

# All containers behind HEAD → exit 1
assert_exit "all containers behind HEAD" 1 \
  "$(setup_mock "$HEAD_SHA" control-api="$OLD_SHA" agent-runner="$OLD_SHA")"

# Container with no git_sha label (pre-label image) → counted as drifted → exit 1
assert_exit "container with no git_sha label" 1 \
  "$(setup_mock "$HEAD_SHA" control-api=HEAD agent-runner="")"

# One absent (skipped), one current, one drifted → exit 1
assert_exit "absent + current + drifted mix" 1 \
  "$(setup_mock "$HEAD_SHA" control-api=absent agent-runner=HEAD frontend="$OLD_SHA")"

# One absent (skipped), one current → exit 0 (absent is not drift)
assert_exit "absent + current (no drift)" 0 \
  "$(setup_mock "$HEAD_SHA" control-api=absent agent-runner=HEAD)"

echo ""
printf "Results: %s passed, %s failed\n" "$PASS" "$FAIL"
[[ "$FAIL" -eq 0 ]]
