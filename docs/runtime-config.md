# Runtime Configuration

Operator runbook for configuring the agent and chat runtime subsystem: environment variables, resource limits, LLM credential storage, and troubleshooting.

**Services covered**: `control-api` (chat-gateway, MCP server), `agent-runner` (runtime supervisor).
**Builds on**: [ADR-009](decisions/ADR-009-agent-execution-architecture.md) (Wave 7), ADR-013 (Wave 9).

---

## Environment variables — control-api

These variables configure the chat gateway and MCP token model on `control-api`.

| Variable | Default | Description |
|----------|---------|-------------|
| `RB_CHAT_PANEL_ENABLED` | `false` | Feature flag. Set `true` to expose `/v1/chat/*` routes and the ChatPage UI. |
| `RB_MCP_JWT_SECRET` | — | **Required for chat.** HS256 signing key for MCP session JWTs. Store in `rb-secrets`; rotate via key-id in the JWT `kid` header. |
| `RB_MCP_JWT_TTL_SECS` | `900` (15 min) | JWT lifetime. Tokens are re-minted on activity when within `RB_MCP_JWT_REFRESH_SECS` of expiry. |
| `RB_MCP_JWT_REFRESH_SECS` | `120` (2 min) | Window before `exp` in which the gateway re-mints the JWT and rewrites `.mcp.json`. |
| `RB_TENANT_SESSION_CAP` | `100` | Maximum concurrent agent+chat sessions per tenant. |
| `RB_SESSION_CREATE_RATE_LIMIT` | `10` | Session-create requests per minute per tenant. |

---

## Environment variables — agent-runner

These variables configure the runtime supervisor and adapters on `agent-runner`.

| Variable | Default | Description |
|----------|---------|-------------|
| `KAFKA_BOOTSTRAP_SERVERS` | `kafka:9092` | Kafka broker list. `agent-runner` consumes from `rb.agent.commands`. |
| `RB_AGENT_WORKSPACE_BASE` | `/data/agent-workspaces` | Root directory for per-session isolated workspaces. Each session gets `{base}/{tenant_id}/{session_id}/`. |
| `RUST_BRAIN_API_BASE` | `http://control-api:8080` | Control-API base URL for internal callbacks. |
| `RB_CONTROL_API_BASE_URL` | `http://control-api:8081` | Internal API base (status callbacks, key revocation). |
| `RB_INTERNAL_SECRET` | — | Shared secret for internal endpoints (`/internal/agent/*`). |
| `AGENT_RUNTIMES_ENABLED` | all | Comma-separated list of enabled runtimes. Set to e.g. `claude_code` to disable others cluster-wide without a code change. |
| `AGENT_DEFAULT_RUNTIME` | `claude_code` | Default runtime when not specified in the session-create request. |
| `AGENT_DEFAULT_MODEL_BY_RUNTIME` | — | JSON map of `{runtime: model}` defaults, e.g. `{"opencode":"claude-sonnet-4-6"}`. |

### Claude Code adapter

| Variable | Default | Description |
|----------|---------|-------------|
| `CLAUDE_CONFIG_DIR` | `/home/loginuser/.claude` | Path to Claude credentials directory. Mounted read-only from the `claude-credentials` named volume. |
| `ANTHROPIC_API_KEY` | — | Direct Anthropic API key (optional; OAuth credentials via `claude-login` are the primary path). |

### OpenCode / LiteLLM adapter

| Variable | Default | Description |
|----------|---------|-------------|
| `LITELLM_BASE_URL` | — | LiteLLM proxy endpoint (e.g. `http://litellm:4000`). Required for `opencode` and `pi` runtimes. |
| `LITELLM_API_KEY` | — | LiteLLM virtual key. One key per (tenant, runtime) tuple. |
| `CHAT_MODEL` | — | LLM model to use for chat sessions via LiteLLM (e.g. `claude-sonnet-4-6`, `gpt-4o`). |

---

## LLM credential storage

### Claude Code (OAuth via SSH sidecar)

The `claude-login` SSH sidecar container provides a one-time login flow. Credentials are stored in the `claude-credentials` Docker named volume and mounted read-only into `agent-runner`.

```bash
# One-time setup: SSH into the sidecar and log in
ssh -p ${CLAUDE_SSH_HOST_PORT:-12222} loginuser@localhost
# Inside the container:
claude /login
```

The credentials persist across container restarts (Docker named volume). Re-run `claude /login` only if the credentials expire or are revoked.

**Alert**: `ClaudeCredentialsMissing` fires when `claude_code` sessions fail with `error_kind="claude_not_logged_in"` at > 10/hour. Re-run `claude /login` via the SSH sidecar.

### LiteLLM provider keys

LLM provider API keys (Anthropic, OpenAI, Bedrock) are stored in `rb-secrets` and mounted into the LiteLLM pod only. They are **never** mounted into `control-api` or `agent-runner`.

```
rb-secrets
  └─ litellm/
       ├─ ANTHROPIC_API_KEY
       ├─ OPENAI_API_KEY
       └─ ...provider keys
```

To add or rotate a provider key:

1. Update the key in `rb-secrets`.
2. Restart the LiteLLM service to pick up the new value.
3. Verify with `curl http://<litellm>:4000/health/readiness`.

### MCP JWT signing key

The `RB_MCP_JWT_SECRET` is the HS256 key used to sign and verify MCP session tokens. It must be stored in `rb-secrets`.

To rotate:

1. Generate a new key (at least 32 bytes of randomness).
2. Update `RB_MCP_JWT_SECRET` in `rb-secrets`.
3. Restart `control-api`. Existing JWTs signed with the old key will fail verification on their next `/mcp` call, forcing the runtime to get a fresh token at the next activity refresh.

---

## Resource limits

### Per-process limits

Each chat or agent session spawns one OS process in an isolated workspace. Resource limits are enforced via cgroup v2 (the container is already cgroup-scoped).

| Limit | Default | Env var | Description |
|-------|---------|---------|-------------|
| Memory | 1 GiB | `RB_AGENT_PROCESS_MEMORY_MAX` | cgroup `memory.max`. OOM kill triggers `session_failed{error_kind="runtime_oom"}`. |
| CPU | 1 core-equiv | `RB_AGENT_PROCESS_CPU_WEIGHT` | cgroup `cpu.weight`. Bursty chat workloads are throttled, not killed. |
| Idle timeout | 15 min | `RB_AGENT_IDLE_TIMEOUT_SECS` | Reaper checks `last_activity_at`. No user message for this duration ends the session. |
| Wall-clock | 60 min | `RB_AGENT_WALL_CLOCK_SECS` | Hard cap per session. Terminal event: `wall_clock_exceeded`. |

### Per-tenant limits

| Limit | Default | Env var | Description |
|-------|---------|---------|-------------|
| Concurrent sessions | 20 (chat) / 100 (agent+chat) | `RB_TENANT_CHAT_SESSION_CAP` / `RB_TENANT_SESSION_CAP` | Excess requests return `429 rate_limit_exceeded`. |
| Session-create rate | 10/min | `RB_SESSION_CREATE_RATE_LIMIT` | Per-tenant burst protection. |
| Cost circuit-breaker | $100/hour | `AGENT_TENANT_COST_PER_HOUR_USD_MICRO_CAP` | Applies to LiteLLM-routed runtimes only. Claude Code spend is on the user's plan. |

### Per-node limits

| Limit | Default | Env var | Description |
|-------|---------|---------|-------------|
| Total live sessions | 200 | `MAX_ACTIVE_SESSIONS_PER_PROCESS` | Per-node semaphore. Excess returns `429`. |

---

## Kafka topics

The runtime subsystem uses two Kafka topics (created by `kafka-init` at stack boot):

| Topic | Partitions | Retention | Purpose |
|-------|-----------|-----------|---------|
| `rb.agent.commands` | 6 | 7 days | Session start/terminate commands dispatched by the chat-gateway |
| `rb.agent.events` | 6 | 7 days | Runtime events relayed back from agent-runner to control-api |

---

## Logs and observability

### Structured logs

Both `control-api` and `agent-runner` emit structured JSON logs. Filter by component:

```bash
# Chat gateway logs (session create, JWT mint, dispatch)
docker compose -f compose/dev.yml logs control-api | grep "chat"

# Runtime supervisor logs (spawn, stdin/stdout, crash, cleanup)
docker compose -f compose/dev.yml logs agent-runner

# Filter by session ID
docker compose -f compose/dev.yml logs agent-runner | grep "<session-id>"
```

### Traces

Every chat session opens a root OTel span `agent.session.run` with:
- `tenant.id`, `user.id`, `session.id`
- Tool calls as child spans: `agent.tool.<name>` with `tool.duration_ms`, `tool.result.size`
- LLM calls as child spans: `agent.llm.call` with `llm.model`, `llm.input_tokens`, `llm.output_tokens`

The `trace_id` (32-hex) is pinned on `chat_sessions.trace_id` at session start. View in Tempo via the UI's trace link or:

```bash
curl -s "http://localhost:3200/api/traces/<trace_id>" | jq .
```

### Audit log

Every MCP tool call is written to `audit.audit_events`:
- `action`: `mcp.tools.call.<tool_name>`
- `args_hash`: SHA-256 of the tool arguments (raw args are never persisted)
- `outcome`: success or error
- `jti`: JWT ID for chat-to-audit correlation

Query the audit log:

```bash
curl -s -H "Authorization: Bearer $ADMIN_KEY" \
  "http://localhost:8080/v1/admin/audit-log?action=mcp.tools.call" | jq .
```

### Metrics and alerts

| Metric / Alert | Description |
|---------------|-------------|
| `agent_sessions_active{runtime}` | Gauge of live sessions per runtime |
| `agent_session_duration_seconds` | Histogram of session wall-clock time |
| `mcp_tool_call_duration_seconds` | Histogram of MCP tool call latency (server-side) |
| `ClaudeCredentialsMissing` | `claude_code` sessions failing with `claude_not_logged_in` > 10/hour |
| `LiteLLMUnreachable` | LiteLLM health check failing for > 2 min |
| `OAuthClaudeRefreshFailureRate` | OAuth token refresh failure rate anomaly (logged at 100%) |
| `AgentEventsPartitionLag` | Kafka partition lag on `rb.agent.events` exceeds threshold |

---

## Troubleshooting

### agent-runner not starting

**Symptom**: `agent-runner` container exits immediately or stays in `restarting`.

**Check**:
1. Kafka is healthy: `docker compose -f compose/dev.yml exec kafka kafka-topics.sh --bootstrap-server localhost:9092 --list`
2. `KAFKA_BOOTSTRAP_SERVERS` is set correctly in `compose/dev.yml`.
3. `RB_AGENT_WORKSPACE_BASE` directory exists and is writable inside the container.

### Claude Code sessions fail with `claude_not_logged_in`

**Cause**: The `claude-credentials` volume is empty or credentials have expired.

**Fix**:
```bash
ssh -p ${CLAUDE_SSH_HOST_PORT:-12222} loginuser@localhost
claude /login
```

Verify credentials exist:
```bash
docker compose -f compose/dev.yml exec agent-runner \
  ls -la /home/loginuser/.claude/credentials.json
```

### OpenCode sessions fail with `llm_unavailable`

**Cause**: LiteLLM proxy is unreachable from `agent-runner`.

**Check**:
1. LiteLLM is running: `curl http://<litellm-host>:4000/health/readiness`
2. `LITELLM_BASE_URL` is set in `agent-runner` environment.
3. Network connectivity: `agent-runner` must reach LiteLLM over `rb-net` or the configured URL.

### Workspace disk filling up

**Cause**: Orphaned workspaces from crashed sessions not cleaned up.

**Fix**: The `workspace_gc` reaper runs periodically and sweeps abandoned workspaces. To force cleanup:

```bash
docker compose -f compose/dev.yml exec agent-runner \
  ls /data/agent-workspaces/
# Identify stale directories (no corresponding active session)
```

### High MCP tool call latency (> 200 ms p95)

**Possible causes**:
1. Neo4j or Qdrant under load — check `docker compose -f compose/dev.yml logs neo4j`.
2. Large graph traversal — `get_callers`/`get_callees` at depth > 5 are expensive.
3. Network latency between `agent-runner` and `control-api` (should be < 1 ms on same host).

---

## Compose configuration reference

The `compose/dev.yml` file defines both `agent-runner` and `claude-login`. Key sections:

```yaml
agent-runner:
  image: ghcr.io/jarnura/rustacean/agent-runner:dev
  environment:
    KAFKA_BOOTSTRAP_SERVERS: kafka:9092
    RB_AGENT_WORKSPACE_BASE: /data/agent-workspaces
    RUST_BRAIN_API_BASE: http://control-api:8080
    RB_CONTROL_API_BASE_URL: http://control-api:8081
    RB_INTERNAL_SECRET: ${RB_INTERNAL_SECRET:-dev-internal-secret-change-me}
    CLAUDE_CONFIG_DIR: /home/loginuser/.claude
    LITELLM_BASE_URL: ${LITELLM_BASE_URL:-}
    LITELLM_API_KEY: ${LITELLM_API_KEY:-}
    CHAT_MODEL: ${CHAT_MODEL:-}
  volumes:
    - agent-workspace-data:/data/agent-workspaces
    - claude-credentials:/home/loginuser/.claude:ro

claude-login:
  image: rustbrain/claude-login:dev
  ports:
    - "${CLAUDE_SSH_HOST_PORT:-12222}:22"
  environment:
    RB_SSH_AUTHORIZED_KEYS: ${RB_SSH_AUTHORIZED_KEYS:-}
    CLAUDE_CONFIG_DIR: /home/loginuser/.claude
  volumes:
    - claude-credentials:/home/loginuser/.claude
```

---

## Related documentation

- [Chat Panel](chat-panel.md) — user-facing overview of the chat feature
- [API Reference — Chat endpoints](api-reference.md#chat-session-endpoints-wave-9) — REST API contract
- [Getting Started — Agent runtime setup](getting-started.md#7-agent-runtime-setup-wave-7) — initial setup instructions
- [ADR-009](decisions/ADR-009-agent-execution-architecture.md) — Wave 7 agent execution architecture
- [Architecture](architecture.md) — system overview and topology diagram
