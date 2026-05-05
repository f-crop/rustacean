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
| Loki `/ready` | `GET http://localhost:13300/ready` → expects 200 |

Ports resolve from `compose/tailscale.env` (`CONTROL_API_HOST_PORT`, `FRONTEND_HOST_PORT`, `LOKI_HOST_PORT`).

## Alert behaviour

- On first failure: creates a Paperclip issue (priority: high, status: todo).
- Cooldown: 30 minutes. Subsequent failures within the cooldown window are logged but do not create duplicate issues.
- Recovery: when all checks pass again, the cooldown state is cleared so the next failure alerts immediately.

## Setup on mars

### 1. Pull the repo

```bash
cd /opt/rustbrain
git pull
```

### 2. Make the script executable

```bash
chmod +x scripts/dev-health-check.sh
```

### 3. Create the Paperclip credentials file

```bash
sudo tee /etc/rustbrain-dev-health.env <<'EOF'
PAPERCLIP_API_URL=http://localhost:3100
PAPERCLIP_API_KEY=<long-lived-token-or-agent-key>
PAPERCLIP_COMPANY_ID=e69fd970-615f-4c94-85f4-f46aa2da8f03
EOF
sudo chmod 600 /etc/rustbrain-dev-health.env
```

> **Note:** The `PAPERCLIP_API_URL` above uses the host port for Paperclip (port 3100 per `docs/PORT_MAP.md`). Use a long-lived API key that has permission to create issues. Never commit credentials to the repo.

### 4. Install the systemd units

```bash
sudo cp infra/systemd/rustbrain-dev-health.service /etc/systemd/system/
sudo cp infra/systemd/rustbrain-dev-health.timer   /etc/systemd/system/
```

Edit `User=` and `WorkingDirectory=` / `ExecStart=` in the `.service` file if the repo is not at `/opt/rustbrain`:

```bash
sudo sed -i 's|/opt/rustbrain|/actual/path|g' /etc/systemd/system/rustbrain-dev-health.service
```

### 5. Enable and start

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now rustbrain-dev-health.timer
sudo systemctl status rustbrain-dev-health.timer
```

### 6. Verify

Run the check once manually:

```bash
sudo systemctl start rustbrain-dev-health.service
journalctl -u rustbrain-dev-health.service --no-pager
```

Expected healthy output:

```
[dev-health-check] 2026-05-06T10:00:00Z: all healthy
```

Expected output when a service is down:

```
[dev-health-check] 2026-05-06T10:00:00Z: ALERT — failures detected:
  - container 'control-api' is not running (project: rustbrain-dev)
  - control-api /health returned HTTP 000 (expected 200) at http://localhost:18080/health
[dev-health-check] Paperclip alert posted (issue: <uuid>)
```

## Monitoring the timer

```bash
# Check timer schedule
systemctl list-timers rustbrain-dev-health.timer

# Tail live logs
journalctl -fu rustbrain-dev-health.service

# Last 20 runs
journalctl -u rustbrain-dev-health.service -n 100 --no-pager
```

## Adjusting parameters

Override via `EnvironmentFile` or `Environment=` in the service unit:

| Variable | Default | Description |
|----------|---------|-------------|
| `COMPOSE_ENV_FILE` | (none) | Path to tailscale.env for port overrides |
| `ALERT_COOLDOWN_SECS` | `1800` | Minimum seconds between Paperclip alerts |
| `RB_STATE_DIR` | `~/.local/state/rustbrain` | State directory for cooldown tracking |

## Disabling temporarily

```bash
sudo systemctl stop rustbrain-dev-health.timer
# ... maintenance ...
sudo systemctl start rustbrain-dev-health.timer
```
