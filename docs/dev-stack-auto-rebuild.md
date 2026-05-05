# Dev-stack Auto-rebuild

When a commit lands on `main` that touches a built service (control-api, frontend), the dev-stack on mars automatically rebuilds the affected images and restarts the containers. UAT always runs against `main` HEAD.

## How it works

Two scripts live in `scripts/`:

| Script | Purpose |
|--------|---------|
| `dev-stack-watch.sh` | Polls `origin/main` every 60 s. When a new SHA appears, calls `dev-stack-auto-rebuild.sh`. |
| `dev-stack-auto-rebuild.sh` | Diffs changed paths, builds only affected images, restarts containers, health-checks, and logs the result. |

### Selective rebuild rules

The rebuild script maps changed file paths to services:

| Changed path | Service rebuilt |
|---|---|
| `services/control-api/**`, `crates/**`, `Cargo.toml`, `Cargo.lock`, `docker/control-api/**`, `migrations/**`, `proto/**` | `control-api` (+ re-runs `rb-migrations`) |
| `frontend/**`, `docker/frontend/**` | `frontend` |
| `compose/dev.yml`, `compose/full.yml`, `compose/tailscale.yml`, `compose/tailscale.env`, `compose/scripts/**` | both services |
| Anything else (docs, `.github/`, governance, …) | **no rebuild** |

Rebuilds are idempotent — re-running is safe. Migrations are always re-run before control-api restarts; they skip already-applied versions.

### Health checks

After restart the script waits 15 s then probes:

- **control-api** — `GET http://localhost:${CONTROL_API_HOST_PORT:-8080}/health` → expects HTTP 200
- **frontend** — `GET http://localhost:${FRONTEND_HOST_PORT:-15173}/` → expects HTTP 200

Results are written to the rebuild log and optionally posted as a GitHub commit status.

## Setup on mars

### 1. Clone or pull the repo

```bash
cd /opt/rustbrain   # or wherever the repo lives on mars
git pull
```

### 2. Make scripts executable

```bash
chmod +x scripts/dev-stack-watch.sh scripts/dev-stack-auto-rebuild.sh
```

### 3. Create the systemd service

```bash
sudo tee /etc/systemd/system/rustbrain-dev-watch.service <<'EOF'
[Unit]
Description=Rustbrain dev-stack auto-rebuild watcher
After=network-online.target docker.service
Wants=network-online.target

[Service]
Type=simple
User=ubuntu
WorkingDirectory=/opt/rustbrain
ExecStart=/opt/rustbrain/scripts/dev-stack-watch.sh /opt/rustbrain
Restart=on-failure
RestartSec=30

# Compose command for mars (Tailscale overlay)
Environment="COMPOSE_CMD=docker compose --env-file /opt/rustbrain/compose/tailscale.env -f /opt/rustbrain/compose/dev.yml -f /opt/rustbrain/compose/tailscale.yml"
Environment="POLL_INTERVAL=60"

# Source the compose env-file into the rebuild script's shell so host-port overrides
# (e.g. CONTROL_API_HOST_PORT=18080, FRONTEND_HOST_PORT=15173) are visible during
# health checks. docker compose's --env-file only reaches containers, not this shell.
Environment="COMPOSE_ENV_FILE=/opt/rustbrain/compose/tailscale.env"

# Optional: post GitHub commit status on rebuild completion
# Environment="GITHUB_TOKEN=ghp_..."
# Environment="GITHUB_REPO=jarnura/rustbrain"

[Install]
WantedBy=multi-user.target
EOF
```

Adjust `User=` and `WorkingDirectory=` / `ExecStart=` paths to match your actual mars layout.

### 4. Enable and start

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now rustbrain-dev-watch
sudo systemctl status rustbrain-dev-watch
```

### 5. Verify

Tail the service log:
```bash
journalctl -fu rustbrain-dev-watch
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
sudo systemctl stop rustbrain-dev-watch
# ... do your manual work ...
sudo systemctl start rustbrain-dev-watch
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

`--cold-start` forces both `control-api` and `frontend` to rebuild, runs migrations, then calls `docker compose up -d` (without `--no-deps`) so databases, queues, and all other infrastructure services start alongside the application containers.

The watcher (`dev-stack-watch.sh`) automatically detects a stopped stack at startup and on each new-commit cycle, and passes `--cold-start` to the rebuild script when needed. The `COMPOSE_CMD` environment variable must be set (or accept the default) for cold-start detection to work correctly.

## Troubleshooting

**Rebuild never triggers**
- Check `journalctl -fu rustbrain-dev-watch` — the watch loop should log every new SHA it sees.
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
