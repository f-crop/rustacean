# Dev-stack Health Alerting

The `rustbrain-dev-health` systemd timer runs `scripts/dev-health-check.sh` every 5 minutes on mars. If critical containers or health endpoints are down it creates a Paperclip issue (priority: high) as an alert.

## What is checked

| Check | Detail |
|-------|--------|
| `postgres` container running | `docker ps` label filter on project `rustbrain-dev` |
| `control-api` container running | same |
| `frontend` container running | same |
| `kafka` container running | same |
| control-api `/health` | `GET http://localhost:18080/health` → expects 200 |
| frontend | `GET http://localhost:15173/` → expects 200 |
| Loki `/ready` | `docker exec rustbrain-dev-loki-1 wget --spider http://localhost:3100/ready` (in-network probe) |
| Crash-loop (all containers) | `RestartCount` delta > 3 within a 10-minute sliding window — catches rapid exit/restart loops |
| Stuck in `created` (all containers) | Container was created > 5 min ago but never started — caused by port bind failure or missing env-file (the real incident: `control-api-1` stuck Created for 2h after `compose up` without `--env-file`) |
| Stuck in `restarting` (all containers) | Container has been in Docker `restarting` state for > 5 min — Docker can't get it to `running` |

Host ports resolve from `compose/tailscale.env` (`CONTROL_API_HOST_PORT`, `FRONTEND_HOST_PORT`). Loki does not publish a host port — the probe runs inside the container, so a Loki check fails iff the container itself is unhealthy.

The last three checks run against **all** `rustbrain-dev` containers (not just the critical four), so silent failures in worker containers (qdrant, loki, projectors, etc.) are also caught.

## Alert behaviour

- On first failure: creates a Paperclip issue (priority: high, status: todo).
- Cooldown: 30 minutes. Subsequent failures within the cooldown window are logged but do not create duplicate issues.
- Recovery: when all checks pass again, the cooldown state is cleared so the next failure alerts immediately.

## Setup on mars

The timer runs as a **user service** (like `rustbrain-dev-watch`) — no `sudo` needed.

### 1. Pull the repo

```bash
cd ~/projects/rustacean
git pull
chmod +x scripts/dev-health-check.sh
```

### 2. Create the Paperclip credentials file

```bash
mkdir -p ~/.config
tee ~/.config/rustbrain-dev-health.env <<'EOF'
PAPERCLIP_API_URL=http://localhost:3100
PAPERCLIP_API_KEY=<long-lived-token-or-agent-key>
PAPERCLIP_COMPANY_ID=e69fd970-615f-4c94-85f4-f46aa2da8f03
EOF
chmod 600 ~/.config/rustbrain-dev-health.env
```

> **Note:** The `PAPERCLIP_API_URL` uses the host port for Paperclip (port 3100 per `docs/PORT_MAP.md`). Use a long-lived API key that has permission to create issues. Never commit credentials to the repo.

### 3. Install the systemd units

```bash
mkdir -p ~/.config/systemd/user
cp infra/systemd/rustbrain-dev-health.service ~/.config/systemd/user/
cp infra/systemd/rustbrain-dev-health.timer   ~/.config/systemd/user/
```

The units use `%h` (systemd home-dir specifier) so no path editing is needed — they resolve to `~/projects/rustacean` automatically.

### 4. Enable and start

```bash
systemctl --user daemon-reload
systemctl --user enable --now rustbrain-dev-health.timer
systemctl --user status rustbrain-dev-health.timer
```

### 5. Verify

Run the check once manually:

```bash
systemctl --user start rustbrain-dev-health.service
journalctl --user -u rustbrain-dev-health.service --no-pager
```

Expected healthy output:

```
[dev-health-check] 2026-05-06T10:00:00Z: all healthy
```

Expected output when a service is down:

```
[dev-health-check] 2026-05-06T10:00:00Z: ALERT — failures detected:
  - container 'control-api' is not running (project: rustbrain-dev)
  - container 'rustbrain-dev-control-api-1' stuck in 'created' state for 7 min — never started (possible port bind or missing env-file)
  - control-api /health returned HTTP 000 (expected 200) at http://localhost:18080/health
[dev-health-check] Paperclip alert posted (issue: <uuid>)
```

## Monitoring the timer

```bash
# Check timer schedule
systemctl --user list-timers rustbrain-dev-health.timer

# Tail live logs
journalctl --user -fu rustbrain-dev-health.service

# Last 20 runs
journalctl --user -u rustbrain-dev-health.service -n 100 --no-pager
```

## Adjusting parameters

Override via `EnvironmentFile` or `Environment=` in the service unit:

| Variable | Default | Description |
|----------|---------|-------------|
| `COMPOSE_ENV_FILE` | (none) | Path to tailscale.env for port overrides |
| `ALERT_COOLDOWN_SECS` | `1800` | Minimum seconds between Paperclip alerts |
| `RB_STATE_DIR` | `~/.local/state/rustbrain` | State directory for cooldown and crash-loop tracking |
| `CRASH_LOOP_WINDOW_SECS` | `600` | Sliding window (seconds) for crash-loop detection |
| `CRASH_LOOP_THRESHOLD` | `3` | Max restart delta within the window before alerting |
| `STUCK_STATE_SECS` | `300` | Seconds a container must be stuck in `created`/`restarting` before alerting |

### State files written by the health check

The script maintains state under `$RB_STATE_DIR` (default `~/.local/state/rustbrain`):

```
~/.local/state/rustbrain/
├── dev-health-alert.last         # epoch of last Paperclip alert (cooldown)
├── restart-counts/
│   └── rustbrain-dev-<svc>-1    # per-container sliding restart-count history
└── stuck-since/
    └── rustbrain-dev-<svc>-1.restarting  # first time we saw this container in restarting state
```

To reset all state (e.g. after resolving an incident):

```bash
rm -rf ~/.local/state/rustbrain
```

## Disabling temporarily

```bash
systemctl --user stop rustbrain-dev-health.timer
# ... maintenance ...
systemctl --user start rustbrain-dev-health.timer
```

## Known pitfall: nginx caches upstream IP at startup

**Symptom**: after the auto-rebuild watcher recreates `control-api` (new Docker bridge IP),
all `/v1/*` requests through the frontend return `502 Bad Gateway` until the frontend
container is manually restarted. nginx error log shows the dead IP:

```
connect() failed (111: Connection refused) ... upstream: "http://172.25.0.XX:8080/v1/..."
```

**Root cause**: plain `proxy_pass http://control-api:8080;` causes nginx to resolve the
hostname once at startup and cache the IP for the container lifetime.

**Fix** (shipped in `docker/frontend/nginx.conf`): use Docker's embedded DNS resolver with
a 10-second TTL and route `proxy_pass` through a variable:

```nginx
resolver 127.0.0.11 valid=10s;
set $upstream control-api:8080;
proxy_pass http://$upstream;
```

When `proxy_pass` references a variable nginx re-resolves via the configured resolver on
each request (within the TTL), so a recreated `control-api` is picked up within 10 seconds
without any manual restart.
