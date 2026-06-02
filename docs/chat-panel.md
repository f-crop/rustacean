# Chat Panel

The chat panel is an interactive coding assistant embedded in the Rustacean UI. A logged-in user opens a chat session, types a message, and watches a coding runtime (Claude Code, OpenCode) answer using the same MCP read tools that the agent execution surface exposes.

Chat sessions are flag-gated, tenant-scoped, and audited. The runtime process runs in an isolated workspace with a short-lived, read-only MCP credential.

**ADR**: ADR-013 (Wave 9 — chat-panel architecture, runtime contract, MCP token model).
**Builds on**: [ADR-009](decisions/ADR-009-agent-execution-architecture.md) (Wave 7 agent execution).

---

## Enabling the chat panel

The chat panel is behind a feature flag. Set `RB_CHAT_PANEL_ENABLED=true` on the `control-api` service and restart:

```bash
# In compose/dev.yml, add to control-api environment:
RB_CHAT_PANEL_ENABLED=true

# Restart control-api
docker compose -f compose/dev.yml restart control-api
```

When the flag is off (the default), the chat routes return 404 and the frontend hides the navigation entry.

---

## How it works

```
Browser (ChatPage)
  │
  ├─ POST /v1/chat/sessions              create a new chat session
  ├─ POST /v1/chat/sessions/{id}/messages send a user message
  └─ GET  /v1/chat/sessions/{id}/events   stream assistant tokens (SSE)
         │
    control-api (chat-gateway)
         │  mints a short-lived MCP JWT (read-only, tenant-bound)
         │  dispatches runtime command via Kafka
         │
    agent-runner (runtime adapter)
         │  spawns one OS process per session (claude_code / opencode)
         │  bridges stdin/stdout with redaction
         │
    POST /mcp (Bearer <JWT>)
         │  runtime calls MCP read tools: search_items, get_item,
         │  get_callers, get_callees, get_trait_impls
         │
    SSE event stream → browser
```

1. **Create session** — `POST /v1/chat/sessions` with `{ "runtime": "claude_code" }`. Returns a session ID and the SSE endpoint URL.
2. **Send message** — `POST /v1/chat/sessions/{id}/messages` with `{ "body": "Find all implementations of RuntimeAdapter" }`. The gateway dispatches the message to the runtime process over Kafka.
3. **Stream response** — `GET /v1/chat/sessions/{id}/events` (SSE). Assistant tokens arrive as `data:` frames. Connect with `EventSource` or the `useEventSource` hook.
4. **End session** — `POST /v1/chat/sessions/{id}/end` or let the idle timeout (15 min) clean up.

---

## Authentication

Chat endpoints accept the same auth as the rest of the API:

- **Session cookie** (`rb_session`) — the default for browser users.
- **Bearer API key** (`Authorization: Bearer rb_...`) — requires `agent` or `admin` scope.

The user must have a verified email and an active tenant membership.

---

## Available runtimes

| Runtime | Binary | Status | Auth source |
|---------|--------|--------|-------------|
| `claude_code` | `claude` | Supported | OAuth via shared `claude-credentials` volume |
| `opencode` | `opencode` | Supported | LiteLLM proxy for multi-provider LLM access |
| `pi` | — | Deferred (Phase 3) | LiteLLM proxy (stub) |

Pass the runtime name in the session-creation request: `{ "runtime": "claude_code" }`.

- **`claude_code`** sessions bypass LiteLLM entirely — unaffected if LiteLLM is down.
- **`opencode`** and **`pi`** sessions fail with `llm_unavailable` if LiteLLM is unreachable.

See [Runtime Configuration](runtime-config.md) for the full operator setup.

---

## MCP tools visible in chat

Chat sessions use a **read-only** MCP tool set. The runtime can call these tools via the MCP server:

| Tool | Description |
|------|-------------|
| `search_items` | Full-text + semantic search over ingested code symbols |
| `get_item` | Fetch a single code item by fully-qualified name |
| `get_callers` | BFS traversal of the call graph (who calls this?) |
| `get_callees` | BFS traversal of the call graph (what does this call?) |
| `get_trait_impls` | List implementations of a trait |

**Not available in chat** (admin-scope only): `run_query` (raw Cypher). Mutating tools are out of scope for Wave 9.

---

## Session lifecycle

```
  create session (first message)
        │
        ▼
    ┌─────────┐   spawn process    ┌─────────┐
    │  active  │──────────────────▶│ running  │
    └─────────┘                    └────┬─────┘
                                        │
                 ┌──────────────────────┼──────────────────┐
                 │                      │                  │
                 ▼                      ▼                  ▼
           ┌──────────┐         ┌────────────┐     ┌──────────┐
           │  ended   │         │   failed   │     │  ended   │
           │ (user)   │         │ (crash/oom)│     │ (timeout)│
           └──────────┘         └────────────┘     └──────────┘
```

- **Active**: session created, process spawned, accepting messages via stdin.
- **Ended**: user ended the session, or idle/wall-clock timeout triggered.
- **Failed**: runtime process crashed, OOM killed, or an unrecoverable error occurred.

Each user message is a `send_input` over stdin to the warm process. The runtime's stdout is parsed, redacted (credentials and JWTs stripped), and streamed back to the client over SSE.

---

## Security model

Chat sessions use a **short-lived JWT** instead of a long-lived API key for MCP tool calls. This bounds the blast radius of a compromised runtime process.

| Property | Value |
|----------|-------|
| Algorithm | HS256 |
| Audience | `rb-mcp` |
| Scope | `["read"]` (read-only tools only) |
| TTL | 15 minutes (configurable via `RB_MCP_JWT_TTL_SECS`) |
| Tenant binding | `tenant_id` claim is server-fixed; MCP server rejects drift |
| Credential location | `.mcp.json` in the isolated workspace (0600 permissions) |
| Log redaction | JWT is never placed in prompts, messages, events, or logs |

A single redaction pass runs on all runtime output **before** it reaches persistence, the SSE stream, or log lines. The redactor strips JWTs, Bearer tokens, API-key prefixes, and the session's exact live token value. If redaction fails, the line is dropped (fail-closed) and a `redaction_failed` event is emitted.

---

## Resource limits

Each chat session runs in an isolated OS process with these limits:

| Limit | Default | Effect when exceeded |
|-------|---------|----------------------|
| Memory | 1 GiB | OOM kill; session fails with `runtime_oom` |
| CPU | 1 core-equiv shares | Throttled, not killed |
| Idle timeout | 15 min (no user message) | Session ends automatically |
| Wall-clock cap | 60 min per session | Hard termination |
| Per-tenant concurrency | 20 live sessions | Excess returns `429 rate_limit_exceeded` |
| Per-node concurrency | 200 live sessions | Excess returns `429 rate_limit_exceeded` |
| Output per turn | 1 MiB streamed | Back-pressure; overflow stored as blob ref |
| Message body | 16 KiB persisted | Truncated to blob ref if exceeded |

See [Runtime Configuration](runtime-config.md) for tunables.

---

## Prompt examples

```
Find all implementations of RuntimeAdapter and show where each is registered.

What does the signup flow look like? Trace from POST /v1/auth/signup through
the transaction.

Show me all callers of write_tool_call_audit — I want to understand the
audit surface.

Search for anything related to tenant deletion and explain the cascade.
```

The runtime has full read access to the tenant's ingested codebase via MCP tools. It cannot modify any data.

---

## Troubleshooting

### Chat panel not visible in the UI

**Cause**: Feature flag is off (default).
**Fix**: Set `RB_CHAT_PANEL_ENABLED=true` on `control-api` and restart.

### Session creation returns 429

**Cause**: Per-tenant concurrency limit (20 active sessions) or per-node limit (200) reached.
**Fix**: End idle sessions, or wait for the idle reaper (15 min) to clean up abandoned sessions.

### Runtime process crashes mid-conversation

**Symptom**: SSE stream emits a `session_failed` event with `error_kind="runtime_crashed"`.
**Cause**: The runtime process exited with a non-zero code or panicked. One crash does not affect other sessions.
**Fix**: Send the message again to start a new turn. Check `agent-runner` logs:

```bash
docker compose -f compose/dev.yml logs agent-runner | grep "runtime_crashed"
```

### MCP tool calls return errors

| Error | Cause | Fix |
|-------|-------|-----|
| `TENANT_DRIFT (-32000)` | Session's tenant changed mid-flight | End the session and create a new one |
| `insufficient_scope (-32601)` | Runtime attempted a write or admin tool | Expected — chat is read-only |
| Tool returns empty results | Tenant has no ingested repositories | Ingest a repository first via the repos UI |

### Cold start is slow (> 4 s)

The first message in a session spawns the runtime process (cold start). Subsequent messages reuse the warm process. Cold-start latency is dominated by the runtime binary's startup time, not Rustacean.

---

## Data retention

Chat data is stored in `control.chat_sessions` and `control.chat_messages`. Message bodies contain only **post-redaction** text — credentials, JWTs, and secrets are stripped before persistence.

- **Tenant deletion**: `ON DELETE CASCADE` to `control.tenants` sweeps all chat data.
- **Retention purge**: terminal sessions older than 90 days are purged nightly (02:00 UTC), matching the `agent_sessions` retention policy.

---

## Related documentation

- [Runtime Configuration](runtime-config.md) — operator runbook for configuring runtimes, resource limits, and LLM credentials
- [API Reference — Chat endpoints](api-reference.md#chat-session-endpoints-wave-9) — REST API contract
- [Getting Started — Use the chat panel](getting-started.md#8-use-the-chat-panel-wave-9) — end-user quickstart
- [ADR-009](decisions/ADR-009-agent-execution-architecture.md) — Wave 7 agent execution architecture (substrate)
- [Architecture](architecture.md) — system overview and topology diagram
