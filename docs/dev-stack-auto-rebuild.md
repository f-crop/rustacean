# Dev-stack Auto-rebuild

When a commit lands on `main` that touches any built service, the dev-stack automatically rebuilds the affected images and restarts the containers. UAT always runs against `main` HEAD.

## Wiring (local dev box)

Locally, the trigger is a git `post-merge` hook checked in under `.githooks/`. Enable it once after cloning:

```bash
scripts/install-git-hooks.sh
```

This sets `core.hooksPath = .githooks` so the `post-merge` hook fires on every `git pull` (or `git merge`) that advances `main`. The hook backgrounds `scripts/dev-stack-auto-rebuild.sh` with `ORIG_HEAD → HEAD`; rebuild logs land in `~/.local/state/rustbrain/post-merge-rebuild.log` and the structured NDJSON log. See [`.githooks/README.md`](../.githooks/README.md) for bypass and troubleshooting.

For unattended boxes (mars), the watcher in `scripts/dev-stack-watch.sh` runs as a systemd service — see *Setup on mars* below.

## How it works

Two scripts live in `scripts/`:

| Script | Purpose |
|--------|---------|
| `dev-stack-watch.sh` | Polls `origin/main` every 60 s. When a new SHA appears, calls `dev-stack-auto-rebuild.sh`. |
| `dev-stack-auto-rebuild.sh` | Diffs changed paths, builds only affected images, restarts containers, health-checks, and logs the result. |

### Selective rebuild rules

The rebuild script maps changed file paths to services:

| Changed path | Services rebuilt |
|---|---|
| `crates/**`, `Cargo.toml`, `Cargo.lock`, `proto/**` | **All 11 Rust services** (shared dependency change) |
| `services/<name>/**`, `docker/<name>/**` | That specific service only |
| `migrations/**` | `control-api` (+ re-runs `rb-migrations`) |
| `frontend/**`, `docker/frontend/**` | `frontend` |
| `compose/dev.yml`, `compose/full.yml`, `compose/tailscale.yml`, `compose/tailscale.env`, `compose/scripts/**` | All services |
| Anything else (docs, `.github/`, governance, …) | **no rebuild** |

The 11 Rust services are: `control-api`, `agent-runner`, `parse-worker`, `typecheck-worker`, `ingest-graph`, `ingest-clone`, `expand-worker`, `embed-worker`, `projector-pg`, `projector-neo4j`, `tombstoner`.

Rebuilds are idempotent — re-running is safe. Migrations are re-run before control-api restarts; they skip already-applied versions.

### Health checks

After restart the script waits 15 s then probes:

- **control-api** — `GET http://localhost:${CONTROL_API_HOST_PORT:-8080}/health` → expects HTTP 200
- **frontend** — `GET http://localhost:${FRONTEND_HOST_PORT:-15173}/` → expects HTTP 200
- **All other Rust services** — `docker inspect` → expects container running

Results are written to the rebuild log and optionally posted as a GitHub commit status.

## Image strategy on mars (local-build-only)

All custom service images (`ghcr.io/jarnura/rustacean/*:dev`, `rustbrain/frontend:dev`) are built
from source on mars. GHCR pull is never used — `compose/dev.yml` sets `pull_policy: never` on
every custom service so Docker will not attempt a registry pull. Third-party images
(postgres, neo4j, kafka, …) are still pulled from their public registries as normal.

**Why**: mars has no GHCR credentials. The `image:` tags in the compose file serve only to
name the locally-built artifact consistently; they are not treated as pull targets.

## Setup on mars

The service is installed as a **user systemd service** (no root required). `loginctl enable-linger jarnura`
is already active on mars, so the user manager starts at boot and the service survives logout.
The checked-in unit file at `infra/systemd/rustbrain-dev-watch.service` uses `%h` specifiers so
it resolves correctly for any user whose repo is at `~/projects/rustacean`.

### 1. Clone or pull the repo

```bash
cd ~/projects/rustacean   # actual path on mars
git pull
```

### 2. Make scripts executable

```bash
chmod +x scripts/dev-stack-watch.sh scripts/dev-stack-auto-rebuild.sh
```

### 3. Build all custom images (first time only)

Because `pull_policy: never` is set, Docker will not pull custom images from a registry.
You must build them locally before the first `docker compose up`:

```bash
export COMPOSE_CMD="docker compose --env-file compose/tailscale.env -f compose/dev.yml -f compose/tailscale.yml"
$COMPOSE_CMD build
```

This tags all 11 custom images locally. Subsequent rebuilds are handled automatically by
`dev-stack-watch.sh` for all services whose source paths change on `main`.

### 4. Install the user systemd service

```bash
mkdir -p ~/.config/systemd/user/
cp infra/systemd/rustbrain-dev-watch.service ~/.config/systemd/user/
```

The unit file uses `%h` specifiers which systemd 248+ expands to the user's home directory
in both path fields (`WorkingDirectory=`, `ExecStart=`) and `Environment=` values.

### 5. Enable and start

```bash
systemctl --user daemon-reload
systemctl --user enable --now rustbrain-dev-watch
systemctl --user status rustbrain-dev-watch
```

### 6. Verify

Tail the service log:
```bash
journalctl --user -fu rustbrain-dev-watch
```

On the next merge to `main` that touches a service, you should see:
```
[dev-stack-watch] new commit: <prev> → <new>
[dev-stack-auto-rebuild] building control-api...
[dev-stack-auto-rebuild] all healthy: control-api=ok
```

## Querying rebuild logs

Each rebuild appends one NDJSON line to `~/.local/state/rustbrain/dev-stack-rebuilds.ndjson`.

```bash
# Show last 10 rebuilds
scripts/dev-stack-auto-rebuild.sh --logs

# Show last 20 rebuilds
scripts/dev-stack-auto-rebuild.sh --logs 20

# Full JSON for detailed inspection
tail -20 ~/.local/state/rustbrain/dev-stack-rebuilds.ndjson | python3 -m json.tool --no-ensure-ascii
```

Each log record:

```json
{
  "timestamp":  "2026-04-30T10:00:00Z",
  "prev_sha":   "abc12345...",
  "new_sha":    "def67890...",
  "rebuilt":    ["control-api"],
  "result":     "ok",
  "health":     "control-api=ok",
  "elapsed_s":  87,
  "reason":     ""
}
```

`result` values: `ok`, `skipped`, `build_failed`, `restart_failed`, `health_failed`.

## Done-gate evidence for code touching `control-api` / `agent-runner`

Per CTO directive (2026-05-12), any PR that touches `services/control-api/` or `services/agent-runner/` must include a `stack-rebuild` line in its Done-gate evidence block. This closes the merged-but-not-deployed gap that has stalled four consecutive Wave 7 UAT rounds.

The evidence block looks like:

```
Done-gate evidence:
- Type: code
- Artifact: https://github.com/f-crop/rustacean/pull/<PR#>
- Verified by: gh pr view <PR#> --json mergedAt,state
- stack-rebuild: control-api restarted at 2026-05-12T09:12:28Z
- stack-rebuild: agent-runner restarted at 2026-05-12T09:12:28Z
```

Rules:

1. **One `stack-rebuild:` line per service touched.** If the PR only touches `control-api`, only `control-api` needs a line. If it touches a shared crate (e.g. `crates/rb-storage-pg`), every Rust service that gets rebuilt counts and must be listed.
2. **Timestamp must be after the PR's `mergedAt`.** The rebuild must observe code that's actually on `main`. Verify with:
   ```bash
   docker inspect <container> --format '{{.State.StartedAt}}'
   ```
3. **Acceptable rebuild sources:**
   - The git `post-merge` hook fired automatically (preferred — see *Wiring* above).
   - A manual `scripts/dev-stack-auto-rebuild.sh <prev_sha> <new_sha>` run.
   - A `--cold-start` after a full stack restart.
4. **`scripts/dev-stack-auto-rebuild.sh --logs 1`** prints the most recent rebuild record; the JSON includes timestamp + service list. Use it to populate the evidence block.

This requirement applies to any code-type issue closed on or after 2026-05-12 whose merged PR touches the named service trees. Issues older than that are grandfathered.

## Bypassing the auto-rebuild for one merge

If you need to merge to `main` without triggering an auto-rebuild (e.g. during a planned outage or while debugging the stack manually):

```bash
# On mars, before the merge lands:
touch /opt/rustbrain/compose/.no-auto-rebuild
```

The watch script will detect this file on the next polling cycle, skip the rebuild, and **delete the file**. One file = one skip. A second merge after that will rebuild normally.

The file is in `.gitignore` territory — do not commit it.

To disable the watcher entirely for a longer window:

```bash
systemctl --user stop rustbrain-dev-watch
# ... do your manual work ...
systemctl --user start rustbrain-dev-watch
```

## Manual rebuild

To trigger a rebuild outside the watch loop (e.g. after a manual `git pull` or to re-apply a failed rebuild):

```bash
export COMPOSE_CMD="docker compose --env-file compose/tailscale.env -f compose/dev.yml -f compose/tailscale.yml"
scripts/dev-stack-auto-rebuild.sh                        # diffs HEAD vs HEAD^1
scripts/dev-stack-auto-rebuild.sh <prev_sha> <new_sha>  # explicit range
```

### Cold start (stack fully stopped)

If the stack has zero running containers (e.g. after `docker compose down` or a machine reboot), use `--cold-start` to skip the diff and bring up the entire stack including infrastructure services:

```bash
export COMPOSE_CMD="docker compose --env-file compose/tailscale.env -f compose/dev.yml -f compose/tailscale.yml"
scripts/dev-stack-auto-rebuild.sh --cold-start                        # builds both services, full up -d
scripts/dev-stack-auto-rebuild.sh --cold-start <prev_sha> <new_sha>  # with explicit SHA range
```

`--cold-start` forces all 11 Rust services and `frontend` to rebuild, runs migrations, then calls `docker compose up -d` (without `--no-deps`) so databases, queues, and all other infrastructure services start alongside the application containers.

The watcher (`dev-stack-watch.sh`) automatically detects a stopped stack at startup and on each new-commit cycle, and passes `--cold-start` to the rebuild script when needed. The `COMPOSE_CMD` environment variable must be set (or accept the default) for cold-start detection to work correctly.

## Reconciling working_dir drift

Each `rustbrain-dev-*` container stores the compose project working directory as a label
(`com.docker.compose.project.working_dir`). Bind-mount paths like `../migrations:/migrations:ro`
resolve **relative to that label**. If the label ever points to a non-canonical path (e.g.
`/tmp/phase2-deploy/compose`) the bind fails when that directory is cleaned up.

**Detect drift:**

```bash
scripts/check-compose-working-dir.sh
```

Prints a per-container `OK` / `DRIFT` report and exits non-zero when any container has a
working_dir label that diverges from `<repo>/compose/`.

**Fix drift (force-recreate):**

```bash
# On mars — use the full tailscale COMPOSE_CMD so ports are correct
COMPOSE_CMD="docker compose --env-file compose/tailscale.env -f compose/dev.yml -f compose/tailscale.yml" \
  scripts/check-compose-working-dir.sh --fix
```

`--fix` runs `docker compose up -d --force-recreate`, which re-creates every container and
re-stamps the correct label. Health checks in the rebuild log confirm recovery.

**Automatic prevention:** `dev-stack-watch.sh` and `dev-stack-auto-rebuild.sh` now refuse to
run when their own directory or `REPO_ROOT` resolves under `/tmp/`; the watcher exits with a
non-zero code and a fatal journal entry, which triggers the `Restart=on-failure` cycle in
systemd. The systemd unit also carries `ConditionPathNotEmpty=…/compose/active-env` so it will
not start at all if that sentinel file is missing or empty.

## Scheduled drift detection

`check-dev-stack-drift.sh` inspects every running container's `git_sha` Docker label and
compares it against `origin/main` HEAD.  Exit codes:

| Exit | Meaning |
|------|---------|
| `0`  | All running containers match HEAD — no drift. |
| `1`  | One or more containers are behind HEAD — drift detected. |
| `2`  | Fatal error (git fetch failed, Docker unavailable, etc.). |

The GitHub Actions workflow `.github/workflows/dev-stack-drift-check.yml` runs this check
every **15 minutes** by SSHing to mars.  When exit 1 is returned:

1. The GHA job **fails** (red check).
2. A GitHub issue tagged `dev-stack-drift` is opened (or an existing one is updated).
3. When the stack catches up, the next passing run auto-closes any open drift alert.

### Required GitHub secrets

| Secret | Value |
|--------|-------|
| `MARS_SSH_KEY`  | Private SSH key that can reach mars. |
| `MARS_SSH_HOST` | Hostname or IP of mars. |
| `MARS_SSH_USER` | SSH username on mars (e.g. `jarnura`). |

Add these via **GitHub → Settings → Secrets → Actions** before the first scheduled run.

### Manual trigger

```bash
# From the GitHub Actions UI:
#   Actions → Dev-stack Drift Check → Run workflow
# or via CLI:
gh workflow run dev-stack-drift-check.yml --repo f-crop/rustacean
```

### Remediation

When a drift alert fires:

```bash
# On mars — force a full rebuild to catch up with origin/main
cd ~/projects/rustacean
scripts/dev-stack-auto-rebuild.sh --cold-start
```

Or restart the auto-rebuild watcher (it will rebuild on the next commit):

```bash
systemctl --user restart rustbrain-dev-watch
```

### Testing exit-code contract

```bash
# Run the shell unit tests (no Docker or live git required):
scripts/test-drift-exit-codes.sh
```

Covers: stack fully down (exit 0), all current (exit 0), one drifted (exit 1),
no-label image (exit 1), mixed absent+drifted (exit 1).

## Troubleshooting

**Rebuild never triggers**
- Check `journalctl --user -fu rustbrain-dev-watch` — the watch loop should log every new SHA it sees.
- Confirm the repo on mars has the remote `origin` pointing at GitHub: `git remote -v`.
- Confirm network access: `git fetch origin main` from the repo directory.

**Health check fails after rebuild**
- Check container logs: `docker compose -f compose/dev.yml logs --tail=50 control-api`
- Look at the rebuild log: `scripts/dev-stack-auto-rebuild.sh --logs 5`
- Manually run: `curl http://localhost:8080/health`

**Build fails**
- Docker build errors are printed inline and recorded in the NDJSON log.
- Run `docker compose -f compose/dev.yml build control-api` manually to see the full output.

**Stack was stopped and watcher didn't bring it back**
- The watcher checks for a cold stack at startup and on each new commit. If no commit landed while the stack was down, trigger manually: `scripts/dev-stack-auto-rebuild.sh --cold-start`
- Check `journalctl -fu rustbrain-dev-watch` to see if the cold-start detection fired at startup.
- Ensure `COMPOSE_CMD` in the systemd unit matches the actual compose file set — the watcher uses this to query running containers.
