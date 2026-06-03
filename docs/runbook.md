# Runbook

Operational reference for the rust-brain dev stack. This document covers start/stop procedures, log access, health verification, common failure modes, and database migrations.

---

## Docker Compose stacks

There are three compose files in `compose/`:

| File | Purpose |
|------|---------|
| `compose/dev.yml` | Full dev stack — all services with default local ports |
| `compose/full.yml` | Full stack including all optional services |
| `compose/test.yml` | Minimal stack used by integration tests |

### Local development

```bash
# Start everything
docker compose -f compose/dev.yml up -d

# Start only infrastructure (skip control-api if running it with cargo)
docker compose -f compose/dev.yml up -d postgres kafka neo4j qdrant \
  otel-collector tempo prometheus grafana ollama

# Stop everything (preserves volumes)
docker compose -f compose/dev.yml down

# Stop and delete all volumes (full reset)
docker compose -f compose/dev.yml down -v
```

---

## Port reference

### Local (default ports)

| Service | Port | Protocol | URL |
|---------|------|----------|-----|
| control-api | 8080 | HTTP | http://localhost:8080 |
| postgres | 5432 | TCP | — |
| kafka | 9094 | TCP (external) | localhost:9094 |
| grafana | 3000 | HTTP | http://localhost:3000 |
| prometheus | 9090 | HTTP | http://localhost:9090 |
| pgweb | 8081 | HTTP | http://localhost:8081 |
| kafka-ui | 8082 | HTTP | http://localhost:8082 |
| caddy | 80/443 | HTTP/S | http://localhost |
| ollama | 11434 | HTTP | http://localhost:11434 |

---

## Health checks

### control-api

```bash
# Liveness — always 200 while the process is running
curl -s http://localhost:8080/health | jq .
# → {"status":"ok"}

# Readiness — 200 when ready to serve traffic (DB connected)
curl -s http://localhost:8080/ready | jq .
# → {"status":"ok"}
```

### PostgreSQL

```bash
docker compose -f compose/dev.yml exec postgres pg_isready -U rustbrain
# → /var/run/postgresql:5432 - accepting connections
```

### Kafka

```bash
docker compose -f compose/dev.yml exec kafka \
  kafka-topics.sh --bootstrap-server localhost:9092 --list
# prints topic names like rb.ingest.clone.commands, rb.projector.events, ...
```

### All service health at once

```bash
docker compose -f compose/dev.yml ps
# STATUS column shows: healthy / running / starting / exited
```

---

## Logs

```bash
# Stream all logs
docker compose -f compose/dev.yml logs -f

# Stream a specific service
docker compose -f compose/dev.yml logs -f control-api

# Last 100 lines from control-api
docker compose -f compose/dev.yml logs --tail=100 control-api

# Filter for errors
docker compose -f compose/dev.yml logs control-api 2>&1 | grep -i error

# Find verification email token in dev (email_transport=console)
docker compose -f compose/dev.yml logs control-api 2>&1 | grep "verify-email"
```

The control-api emits structured JSON logs. To pretty-print them:

```bash
docker compose -f compose/dev.yml logs -f control-api | jq .
```

---

## Database migrations

Migrations are managed by the `migrate` binary in `services/migrate/`. There is no automatic migration on API startup — run migrations explicitly.

### Running migrations

```bash
# Against local Docker postgres
RB_DATABASE_URL=postgres://rustbrain:rustbrain@localhost:5432/rustbrain \
  cargo run -p migrate -- up
```

### Checking migration status

```bash
RB_DATABASE_URL=postgres://rustbrain:rustbrain@localhost:5432/rustbrain \
  cargo run -p migrate -- status
```

### Kafka topic creation

The `kafka-init` container in `dev.yml` creates all required topics on first boot. To re-create topics manually (e.g. after a full `down -v`):

```bash
docker compose -f compose/dev.yml up kafka-init
```

Required topics: `rb.ingest.clone.commands`, `rb.ingest.expand.commands`, `rb.ingest.parse.commands`, `rb.ingest.typecheck.commands`, `rb.ingest.graph.commands`, `rb.ingest.embed.commands`, `rb.projector.events`, `rb.audit.events`.

---

## Browsing the database

**pgweb** is a read-only web-based database browser included in the compose stack.

- Local: http://localhost:8081
- Remote: http://100.87.157.74:18081

Useful queries:

```sql
-- List all tenants
SELECT id, slug, name, status, created_at FROM control.tenants;

-- List all users with verification status
SELECT id, email, email_verified_at IS NOT NULL AS verified, status, created_at
FROM control.users;

-- Active sessions
SELECT id, user_id, tenant_id, expires_at FROM control.sessions
WHERE revoked_at IS NULL AND expires_at > now();

-- API keys for a tenant
SELECT id, name, scopes, last_used_at, revoked_at FROM control.api_keys
WHERE tenant_id = '<uuid>';

-- Auth event log (last 50)
SELECT event, user_id, tenant_id, occurred_at FROM control.auth_events
ORDER BY occurred_at DESC LIMIT 50;
```

---

## Building and deploying the control-api image

The API has a multi-stage Dockerfile at `docker/control-api/Dockerfile`.

```bash
# Build the image
docker build -f docker/control-api/Dockerfile -t rustbrain/control-api:dev .

# Force rebuild in compose
docker compose -f compose/dev.yml build control-api
docker compose -f compose/dev.yml up -d control-api
```

---

## Common failure modes

### control-api exits immediately on start

**Symptom**: `docker compose ps` shows `control-api` as `exited (1)`.

**Diagnosis**:
```bash
docker compose -f compose/dev.yml logs control-api
```

Most common cause: `RB_DATABASE_URL` is wrong or postgres is not yet healthy.

**Fix**: Wait for postgres to be healthy, then restart:
```bash
docker compose -f compose/dev.yml up -d control-api
```

---

### `cargo run -p migrate` fails with "connection refused"

**Cause**: postgres container is not running or the port mapping differs.

**Fix**:
```bash
docker compose -f compose/dev.yml up -d postgres
# Wait for healthy, then re-run migrate
```

---

### Kafka topics already exist error from kafka-init

**Cause**: kafka-init uses `--if-not-exists`; this error should not occur unless the container is run in an unexpected mode.

**Fix**: Check kafka logs, then run `kafka-init` again:
```bash
docker compose -f compose/dev.yml up kafka-init
```

---

### Login returns 429 (rate limited)

**Cause**: ≥ 5 failed login attempts for the same email within 10 minutes.

**Fix**: Wait 15 minutes, or restart the control-api to clear the in-memory rate limiter:
```bash
docker compose -f compose/dev.yml restart control-api
```

---

### Email verification link expired

**Cause**: Email tokens expire after 1 hour. Password reset tokens expire after 15 minutes.

**Fix**: Repeat the signup or forgot-password flow to get a fresh token. In dev, the link appears in the API logs:
```bash
docker compose -f compose/dev.yml logs control-api | grep -E "verify-email|reset-password"
```

---

### OpenAPI schema drift (CI failure)

**Symptom**: CI job `openapi-sync` fails.

**Cause**: A handler was changed without regenerating `openapi.json`.

**Fix**:
```bash
cargo run -p control-api -- print-openapi > openapi.json
git add openapi.json
git commit -m "docs: regenerate openapi.json"
```

---

## Observability

### Grafana dashboards

- Local: http://localhost:3000 (no login required — anonymous access enabled)
- Remote: http://100.87.157.74:13000

Pre-provisioned data sources: Prometheus, Tempo.

### Prometheus metrics

- Local: http://localhost:9090
- Remote: http://100.87.157.74:19090

### Distributed traces (Tempo)

Traces are emitted by the control-api via OTLP gRPC to `otel-collector:4317`, which forwards them to Tempo. View traces in Grafana → Explore → Tempo data source.

### Kafka monitoring

- Local: http://localhost:8082
- Remote: http://100.87.157.74:18082

---

## Live ingest smoke test (ADR-006 §17)

Verifies the full `kafka.produce → kafka.consume → sse.publish` trace chain end-to-end.
Requires only `docker` + `docker compose` — no host Rust toolchain needed.

```bash
# 1. Build (or rebuild) the control-api image to include rb-test-producer
docker compose -f compose/full.yml build control-api

# 2. Start the full stack
docker compose -f compose/full.yml up -d

# 3. Run the smoke harness
scripts/ingest-smoke.sh

# Optional: override tenant or topic
TENANT_ID=<uuid> KAFKA_TOPIC=rb.projector.events scripts/ingest-smoke.sh
```

After the producer step, verify in Grafana Tempo (`http://localhost:13000` on tailscale, `http://localhost:3000` locally): search by tag `rb.tenant_id=<uuid>` and confirm a single trace containing `kafka.produce`, `kafka.consume`, and `sse.publish` spans.

Port reference for verification steps (tailscale env):

| Step | URL |
|------|-----|
| Auth login / SSE stream | http://localhost:10080 (Caddy proxy) |
| Grafana / Tempo | http://localhost:13000 |

---

## Wave 9: Chat panel operations

The chat panel (Wave 9) adds an interactive coding assistant that runs runtime processes inside `agent-runner`. This section covers operator tasks specific to the chat subsystem.

### Feature flag

The chat panel is disabled by default. Enable it on `control-api`:

```bash
# compose/dev.yml — control-api environment
RB_CHAT_PANEL_ENABLED=true

docker compose -f compose/dev.yml restart control-api
```

Verify the flag is active:

```bash
curl -s http://localhost:8080/health | jq '.features.chat_panel'
# → true
```

### MCP JWT secret management

Chat sessions use a short-lived JWT signed with `RB_MCP_JWT_SECRET`. This secret lives in `rb-secrets`.

```bash
# Rotate the JWT signing key:
# 1. Generate a new secret
openssl rand -base64 32

# 2. Set it in compose/dev.yml (control-api environment)
RB_MCP_JWT_SECRET=<new-secret>

# 3. Restart control-api (mints new tokens immediately)
docker compose -f compose/dev.yml restart control-api
```

| Variable | Default | Description |
|----------|---------|-------------|
| `RB_MCP_JWT_SECRET` | — (**required** when chat enabled) | HS256 signing key for MCP chat tokens |
| `RB_MCP_JWT_TTL_SECS` | `900` | Token TTL in seconds (15 min) |
| `RB_MCP_JWT_REFRESH_SECS` | `120` | Re-mint window before expiry |

Key rotation is hitless: set a new secret with a new `kid`; the verifier accepts both during a grace period equal to the old TTL.

### Runtime process inspection

Each chat session runs one OS process inside the `agent-runner` container.

```bash
# List live runtime processes
docker compose -f compose/dev.yml exec agent-runner ps aux | grep -E 'claude|opencode'

# Check active chat sessions in the database
docker compose -f compose/dev.yml exec postgres psql -U rustbrain -c \
  "SELECT id, runtime, status, last_activity_at FROM control.chat_sessions WHERE status = 'active';"

# Count sessions per tenant
docker compose -f compose/dev.yml exec postgres psql -U rustbrain -c \
  "SELECT tenant_id, count(*) FROM control.chat_sessions WHERE status = 'active' GROUP BY tenant_id;"
```

### Common failure modes

#### Chat session returns 503 / feature not available

**Cause**: `RB_CHAT_PANEL_ENABLED` is not set or set to `false`.

**Fix**: Set the flag and restart control-api.

#### JWT verification fails on `/mcp` calls

**Symptom**: Runtime logs show `JwtError::InvalidSignature` or `JwtError::ExpiredToken`.

**Cause**: `RB_MCP_JWT_SECRET` mismatch between control-api (minter) and the `/mcp` verifier (same process, but check for stale env after a partial restart), or the token expired because the session was idle longer than `RB_MCP_JWT_TTL_SECS`.

**Fix**: Ensure `RB_MCP_JWT_SECRET` is identical across a full restart. For expired tokens, the gateway auto-refreshes on the next user message.

#### Runtime process OOM-killed

**Symptom**: `session_failed{error_kind="runtime_oom"}` in agent-runner logs.

**Cause**: The runtime process exceeded the 1 GiB memory limit (cgroup v2 `memory.max`).

**Fix**: This is expected for memory-intensive operations. The user sees a typed error and can retry. If frequent, consider raising the limit in the cgroup configuration.

#### Orphaned runtime processes

**Symptom**: `ps aux` shows runtime processes with no matching active `chat_session`.

**Cause**: Supervisor crash or unclean shutdown.

**Fix**: The existing `workspace_gc` reaper cleans up orphaned workspaces. For processes, restart `agent-runner`:

```bash
docker compose -f compose/dev.yml restart agent-runner
```

#### Redaction failure (line dropped)

**Symptom**: `error_kind="redaction_failed"` events in agent-runner logs; user sees missing lines in the transcript.

**Cause**: The redaction pass encountered an error processing a runtime output line. The line was dropped (fail-closed) rather than persisted with raw credentials.

**Fix**: Check agent-runner logs for the specific redaction error. This is a safety mechanism — the dropped line may have contained sensitive data.
