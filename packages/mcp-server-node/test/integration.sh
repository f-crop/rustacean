#!/usr/bin/env bash
# Integration test: send MCP messages against a local control-api instance.
#
# Prerequisites:
#   - control-api running at $RB_AGENT_API_BASE (default: http://localhost:3000)
#   - RB_AGENT_API_KEY set to a valid API key
#   - `rustbrain-mcp` binary built (npm run build)
#
# Usage:
#   RB_AGENT_API_KEY=<key> bash test/integration.sh
#   RB_AGENT_API_KEY=<key> RB_AGENT_API_BASE=http://localhost:3000 bash test/integration.sh

set -euo pipefail

API_BASE="${RB_AGENT_API_BASE:-http://localhost:3000}"
export RB_AGENT_API_BASE="$API_BASE"

if [[ -z "${RB_AGENT_API_KEY:-}" ]]; then
  echo "ERROR: RB_AGENT_API_KEY is required" >&2
  exit 1
fi

BIN="./node_modules/.bin/rustbrain-mcp"
if [[ ! -f "dist/src/cli.js" ]]; then
  echo "ERROR: dist/src/cli.js not found — run npm run build first" >&2
  exit 1
fi
BIN="node dist/src/cli.js"

PASS=0
FAIL=0

check() {
  local desc="$1"
  local got="$2"
  local want="$3"
  if echo "$got" | grep -q "$want"; then
    echo "  PASS: $desc"
    ((PASS++)) || true
  else
    echo "  FAIL: $desc"
    echo "    wanted pattern: $want"
    echo "    got: $got"
    ((FAIL++)) || true
  fi
}

echo "=== Rustbrain MCP integration test ==="
echo "API base: $API_BASE"
echo ""

# Build message sequence
INIT='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","clientInfo":{"name":"integration-test","version":"0.0.1"}}}'
INITIALIZED='{"jsonrpc":"2.0","method":"notifications/initialized"}'
TOOLS_LIST='{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'
SEARCH='{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search_items","arguments":{"query":"test","limit":1}}}'

MESSAGES="$INIT
$INITIALIZED
$TOOLS_LIST
$SEARCH"

echo "--- Sending messages ---"
OUTPUT=$(echo "$MESSAGES" | $BIN 2>/tmp/mcp-stderr.txt || true)
STDERR=$(cat /tmp/mcp-stderr.txt)

echo "stdout:"
echo "$OUTPUT"
echo ""
echo "stderr:"
echo "$STDERR"
echo ""

# Parse responses (skip blank lines)
RESP1=$(echo "$OUTPUT" | sed -n '1p')
RESP2=$(echo "$OUTPUT" | sed -n '2p')
RESP3=$(echo "$OUTPUT" | sed -n '3p')

echo "--- Assertions ---"
check "initialize returns protocolVersion" "$RESP1" '"protocolVersion"'
check "initialize has serverInfo" "$RESP1" '"serverInfo"'
# notifications/initialized → 202 Accepted, no JSON body written

check "tools/list returns tools array" "$RESP2" '"tools"'
check "tools/list has search_items" "$RESP2" '"search_items"'
check "tools/list has get_item" "$RESP2" '"get_item"'
check "tools/call search_items returns 200" "$RESP3" '"content"'

echo ""
if [[ $FAIL -eq 0 ]]; then
  echo "=== ALL PASSED ($PASS checks) ==="
  exit 0
else
  echo "=== FAILED: $FAIL/$((PASS + FAIL)) checks ==="
  exit 1
fi
