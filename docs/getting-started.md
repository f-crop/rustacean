# Getting Started

This guide takes you from zero to a running rust-brain dev environment with a verified user account.

---

## Prerequisites

| Tool | Minimum version | Install |
|------|----------------|---------|
| Docker | 24.x + Compose V2 | https://docs.docker.com/get-docker/ |
| Rust | 1.85 | `curl https://sh.rustup.rs -sSf \| sh` |
| Node.js | 20 LTS | https://nodejs.org/ |
| Git | any recent | system package manager |

Verify your environment:

```bash
docker compose version   # should print v2.x
rustc --version          # should print 1.85 or later
node --version           # should print v20 or later
```

---

## 1. Clone the repository

```bash
git clone https://github.com/jarnura/rustacean.git
cd rustacean
```

---

## 2. Start the infrastructure stack

`compose/dev.yml` defines every service the application needs — PostgreSQL, Kafka, Neo4j, Qdrant, OpenTelemetry Collector, Tempo, Prometheus, Grafana, Ollama, Caddy, pgweb, Kafka UI, and the control-api itself.

```bash
docker compose -f compose/dev.yml up -d
```

Wait for services to become healthy (about 30–60 seconds):

```bash
docker compose -f compose/dev.yml ps
```

All services should show `healthy` or `running`. Postgres and Kafka take the longest.

### Verifying infrastructure health

```bash
# Postgres
docker compose -f compose/dev.yml exec postgres pg_isready -U rustbrain

# Kafka
docker compose -f compose/dev.yml exec kafka \
  kafka-topics.sh --bootstrap-server localhost:9092 --list

# control-api
curl -s http://localhost:8080/health | jq .
# → {"status":"ok"}
```

---

## 3. Run database migrations

The `migrate` service runs SQL migrations against the `control` schema and creates required Kafka topics. Run it once after first boot and after any schema-changing PR is merged:

```bash
RB_DATABASE_URL=postgres://rustbrain:rustbrain@localhost:5432/rustbrain \
  cargo run -p migrate -- up
```

You should see output like:

```
[migrate] running control schema migrations...
[migrate] applied: 20240101_create_control_schema.sql
...
[migrate] all migrations applied
```

To check current migration state:

```bash
RB_DATABASE_URL=postgres://rustbrain:rustbrain@localhost:5432/rustbrain \
  cargo run -p migrate -- status
```

---

## 4. Configure environment variables

The control-api reads configuration from environment variables. The Docker Compose file sets defaults suitable for local development — you only need to override them for custom setups.

For running the API **outside Docker** (e.g. `cargo run`), create a `.env` file or export the following:

```bash
export RB_DATABASE_URL=postgres://rustbrain:rustbrain@localhost:5432/rustbrain
export RB_LISTEN_ADDR=0.0.0.0:8080
export RB_BASE_URL=http://localhost:8080
export RB_CORS_ORIGINS=http://localhost:15173
export RB_EMAIL_TRANSPORT=console        # prints emails to stdout
export RUST_LOG=info,control_api=debug
```

Full variable reference: [docs/api-reference.md — Environment Variables](api-reference.md#environment-variables).

### Code intelligence (optional)

The code intelligence features (semantic search, call-graph traversal, trait impls, type usages) require additional infrastructure. Docker Compose starts these services automatically; if running the API outside Docker, set:

```bash
export RB_NEO4J_URI=bolt://localhost:7687      # enables graph endpoints (callers, callees, impls, usages)
export RB_NEO4J_PASSWORD=neo4j
export RB_QDRANT_URL=http://localhost:6333      # enables POST /v1/search
export RB_OLLAMA_URL=http://localhost:11434      # required for search (query embedding)
export RB_EMBEDDING_MODEL=nomic-embed-text      # must match embed-worker model
```

When these variables are absent, the corresponding endpoints return 503 — the rest of the API works normally.

---

## 5. Start the frontend dev server

The frontend is a React 18 + Vite app in `frontend/`. It proxies `/v1`, `/health`, and `/ready` to the control-api.

```bash
cd frontend
npm install
npm run dev
```

The dev server starts at `http://localhost:15173`.

If the API is running on a different address, override the proxy target:

```bash
# frontend/.env.local
VITE_API_BASE_URL=http://localhost:8080
```

---

## 6. Verify it works: sign up → verify → log in

### Option A — using the UI

1. Open `http://localhost:15173` in your browser.
2. Click **Sign up** and fill in email, password (min 12 chars), and a workspace name.
3. Check the control-api logs for the verification email link (transport is `console` in dev):
   ```bash
   docker compose -f compose/dev.yml logs control-api | grep "verify-email"
   ```
4. Copy the `?token=...` value and POST it:
   ```bash
   curl -s -X POST http://localhost:8080/v1/auth/verify-email \
     -H 'Content-Type: application/json' \
     -d '{"token":"<token-from-log>"}'
   # → 204 No Content
   ```
5. Log in at `http://localhost:15173/login` — you should land on the repositories page.

### Option B — curl end-to-end

```bash
# Sign up
curl -s -c cookies.txt -X POST http://localhost:8080/v1/auth/signup \
  -H 'Content-Type: application/json' \
  -d '{
    "email": "alice@example.com",
    "password": "correct-horse-battery",
    "tenant_name": "Acme Corp"
  }' | jq .
# → {"email_verification_required":true,"user_id":"<uuid>"}

# Grab the verification token from API logs
docker compose -f compose/dev.yml logs control-api 2>&1 | grep verify-email | tail -1

# Verify email
curl -s -X POST http://localhost:8080/v1/auth/verify-email \
  -H 'Content-Type: application/json' \
  -d '{"token":"<token>"}' -o /dev/null -w "%{http_code}"
# → 204

# Log in
curl -s -c cookies.txt -X POST http://localhost:8080/v1/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"email":"alice@example.com","password":"correct-horse-battery"}' | jq .
# → {"user_id":"...","tenant_id":"...","email_verification_required":false}

# Fetch your profile
curl -s -b cookies.txt http://localhost:8080/v1/me | jq .
```

---

## 7. Agent runtime setup (Wave 7)

The agent execution subsystem requires two additional components.

### claude-login sidecar

The `claude-login` container provides an SSH endpoint for a one-time `claude /login` to obtain OAuth credentials. These are stored in the `claude-credentials` named volume and mounted read-only into `agent-runner`.

```bash
# SSH into the sidecar and run the interactive login (one-time)
ssh -p ${CLAUDE_SSH_HOST_PORT:-12222} loginuser@localhost
# Inside the container:
claude /login
```

See [runbooks/claude-login.md](runbooks/claude-login.md) for troubleshooting.

### Dev-stack auto-rebuild watcher

Install the git hooks that trigger automatic selective rebuilds on `git pull`:

```bash
./scripts/install-git-hooks.sh
```

See [ADR-011](decisions/ADR-011-dev-stack-auto-rebuild.md) and [runbooks/stack-rebuild-verify.md](runbooks/stack-rebuild-verify.md).

### LiteLLM (OpenCode runtime)

When using the `opencode` adapter, the agent-runner connects to an external LiteLLM proxy. Set these env vars in your compose override or `.env`:

```bash
LITELLM_BASE_URL=http://<litellm-host>:4000   # LiteLLM proxy endpoint
LITELLM_API_KEY=sk-...                         # LiteLLM virtual key
```

LiteLLM is not included in `compose/dev.yml` — it runs as an external service on mars in v1 (see [ADR-009 §3.4](decisions/ADR-009-agent-execution-architecture.md)).

---

## 8. Use the chat panel (Wave 9)

The chat panel lets you interact with an AI coding assistant that has read access to your ingested codebase via MCP tools. It requires the agent runtime ([§7](#7-agent-runtime-setup-wave-7)) to be set up first.

### Enable the feature flag

The chat panel is off by default. Enable it on `control-api`:

```bash
# Add to compose/dev.yml under control-api → environment:
RB_CHAT_PANEL_ENABLED=true

# Restart control-api to pick up the flag
docker compose -f compose/dev.yml restart control-api
```

### Start a chat session

**Option A — using the UI**

1. Open `http://localhost:15173` and log in.
2. Click **Chat** in the sidebar navigation.
3. Select a runtime (`claude_code` or `opencode`) and type a message.
4. Watch the assistant's response stream in real time.

**Option B — curl end-to-end**

```bash
# Create a chat session
curl -s -b cookies.txt -X POST http://localhost:8080/v1/chat/sessions \
  -H 'Content-Type: application/json' \
  -d '{"runtime":"claude_code"}' | jq .
# → {"id":"<session-id>","status":"active","runtime":"claude_code",...}

# Send a message
curl -s -b cookies.txt -X POST \
  http://localhost:8080/v1/chat/sessions/<session-id>/messages \
  -H 'Content-Type: application/json' \
  -d '{"body":"Find all implementations of RuntimeAdapter"}' | jq .

# Stream the response (SSE)
curl -s -b cookies.txt -N \
  http://localhost:8080/v1/chat/sessions/<session-id>/events
# → data: {"type":"message","body":"I found 3 implementations..."}

# End the session
curl -s -b cookies.txt -X POST \
  http://localhost:8080/v1/chat/sessions/<session-id>/end
```

### Prerequisites

- A running dev stack with `control-api` and `agent-runner` healthy.
- At least one repository ingested (the chat uses your codebase data).
- For `claude_code` runtime: Claude credentials set up via the `claude-login` sidecar ([§7](#7-agent-runtime-setup-wave-7)).
- For `opencode` runtime: LiteLLM configured ([§7](#7-agent-runtime-setup-wave-7)).

For detailed configuration: [chat-panel.md](chat-panel.md) · [runtime-config.md](runtime-config.md).

---

## Next steps

- **Architecture deep-dive**: [architecture.md](architecture.md) — system overview, crate layout, agent execution topology
- **Ops reference**: [runbook.md](runbook.md) · [runbooks/](runbooks/) for per-subsystem playbooks
- **API reference**: [api-reference.md](api-reference.md) — all endpoints including agents, MCP, and code intelligence
- **Decision records**: [decisions/](decisions/) — ADR-009 (agent execution), ADR-010 (GitHub install), ADR-011 (auto-rebuild)
- **Contributing**: a contributor guide is forthcoming.
