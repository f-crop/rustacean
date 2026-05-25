# Stack Rebuild Verify

Verify that the mars dev/UAT stack is running the expected code after a merge to
`main`. Covers the auto-rebuild watcher, health endpoints, and SHA pairing.

For the design context behind the auto-rebuild watcher and canonical compose-dir
invariant, see
[ADR-011](../decisions/ADR-011-dev-stack-auto-rebuild.md) and
[dev-stack-auto-rebuild.md](../dev-stack-auto-rebuild.md).

## Audience and when to run

Operators and agents verifying that a merge to `main` was deployed to the mars
dev stack. Run after any PR merge that touches services, crates, or compose
files. Also run as a sanity check when UAT results seem stale or unexpected.

## Prerequisites

- SSH or local shell access to mars (`~/projects/rustacean`)
- The `dev-stack-watch.service` user systemd unit is enabled
- The `compose/active-env` sentinel file exists and is non-empty
- The `COMPOSE_CMD` environment variable is set (or accept the default)

```bash
export COMPOSE_CMD="docker compose --env-file compose/tailscale.env \
  -f compose/dev.yml -f compose/tailscale.yml"
```

## Steps

### 1. Pull the latest code

```bash
cd ~/projects/rustacean
git pull origin main
```

Record the expected SHA:

```bash
EXPECTED_SHA=$(git rev-parse HEAD)
echo "Expected SHA: $EXPECTED_SHA"
```

### 2. Confirm the watcher is active

```bash
systemctl --user status rustbrain-dev-watch
```

Expected: `active (running)`. If inactive, start it:

```bash
systemctl --user start rustbrain-dev-watch
```

Check the `compose/active-env` sentinel:

```bash
cat compose/active-env
```

Expected: `tailscale.env` (or the active environment name). If missing or empty,
the watcher will refuse to start.

### 3. Tail the rebuild log

If a rebuild is in progress (the merge touched service code), monitor it:

```bash
journalctl --user -fu rustbrain-dev-watch
```

Look for:

```
[dev-stack-watch] new commit: <prev-sha> → <new-sha>
[dev-stack-auto-rebuild] building control-api...
[dev-stack-auto-rebuild] all healthy: control-api=ok
```

For doc-only or governance-only merges, the watcher detects no rebuild-worthy
changes and skips:

```
[dev-stack-auto-rebuild] no services affected — skipping rebuild
```

### 4. Spot-check health and build SHA

```bash
# Health probe — all stores should report ok
curl -s http://localhost:18080/health | jq .

# Build SHA — should match EXPECTED_SHA (only after a rebuild)
curl -s http://localhost:18080/health/build | jq .
```

Expected `/health/build` response:

```json
{
  "sha": "<EXPECTED_SHA>",
  "timestamp": "2026-...",
  "dirty": "false"
}
```

If the SHA does not match `$EXPECTED_SHA`, the stack is running stale code.
This is expected when the merge was doc-only (no rebuild triggered). For
service-touching merges, a mismatch means the rebuild failed or has not
completed yet — check the watcher logs (Step 3).

Compare programmatically:

```bash
RUNNING_SHA=$(curl -s http://localhost:18080/health/build | jq -r .sha)
if [ "$RUNNING_SHA" = "$EXPECTED_SHA" ]; then
  echo "SHA match: OK"
else
  echo "SHA MISMATCH: running=$RUNNING_SHA expected=$EXPECTED_SHA"
fi
```

### 5. Check rebuild history

```bash
scripts/dev-stack-auto-rebuild.sh --logs 5
```

Each NDJSON record shows `prev_sha`, `new_sha`, `rebuilt[]`, `result`, and
`health`. Confirm the most recent record matches the expected SHA range.

For detailed JSON:

```bash
tail -5 ~/.local/state/rustbrain/dev-stack-rebuilds.ndjson \
  | python3 -m json.tool --no-ensure-ascii
```

### 6. Verify container start times (optional)

Confirm containers restarted after the merge:

```bash
MERGED_AT=$(gh pr list --repo f-crop/rustacean --state merged --limit 1 \
  --json mergedAt --jq '.[0].mergedAt')

docker inspect rustbrain-dev-control-api-1 \
  --format '{{.State.StartedAt}}'

docker inspect rustbrain-dev-agent-runner-1 \
  --format '{{.State.StartedAt}}'
```

Container start times should be after `$MERGED_AT`.

## Rollback / abort

### Halt a rebuild in progress

If a rebuild is causing issues (e.g., a broken image is crashing on startup):

```bash
# Stop the watcher so it does not retry
systemctl --user stop rustbrain-dev-watch

# Stop all containers
$COMPOSE_CMD down
```

### Manually rebuild a single service

```bash
$COMPOSE_CMD build control-api
$COMPOSE_CMD up -d control-api
```

### Skip one auto-rebuild cycle

To let the next merge land without triggering a rebuild:

```bash
touch /opt/rustbrain/compose/.no-auto-rebuild
```

The watcher consumes and deletes this file on the next polling cycle. One file =
one skip.

### Cold start (stack fully stopped)

After `docker compose down` or a machine reboot:

```bash
export COMPOSE_CMD="docker compose --env-file compose/tailscale.env \
  -f compose/dev.yml -f compose/tailscale.yml"
scripts/dev-stack-auto-rebuild.sh --cold-start
```

This rebuilds all 11 Rust services + frontend, runs migrations, and brings up
the full stack including infrastructure services (postgres, neo4j, kafka, etc.).

### Fix compose working_dir drift

If containers have a stale `com.docker.compose.project.working_dir` label
(caused by running compose from a non-canonical directory):

```bash
# Detect drift
scripts/check-compose-working-dir.sh

# Fix drift (force-recreate all containers)
COMPOSE_CMD="docker compose --env-file compose/tailscale.env \
  -f compose/dev.yml -f compose/tailscale.yml" \
  scripts/check-compose-working-dir.sh --fix
```

## Verification

```bash
# 1. Health endpoint returns ok
curl -s http://localhost:18080/health | jq -e '.status == "ok"'

# 2. Build SHA matches HEAD (for service-touching merges)
EXPECTED_SHA=$(git rev-parse HEAD)
RUNNING_SHA=$(curl -s http://localhost:18080/health/build | jq -r .sha)
[ "$RUNNING_SHA" = "$EXPECTED_SHA" ] && echo "PASS" || echo "FAIL: $RUNNING_SHA"

# 3. Watcher is running
systemctl --user is-active rustbrain-dev-watch

# 4. Canary endpoint responds
curl -s -o /dev/null -w "%{http_code}" http://localhost:15173/
```

Pass criterion: health is `ok`, SHA matches (for service merges), watcher is
`active`, frontend returns HTTP 200.

## Verification record

| Field | Value |
|-------|-------|
| Exercised on | mars (100.87.157.74) |
| Date | 2026-05-25T11:43:00Z |
| Git SHA | `b9b1ca47` (`main` HEAD at exercise time) |
| Operator | Technical Writer agent |
