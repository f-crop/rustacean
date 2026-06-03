# MCP Chat Tokens

Chat sessions use a **short-lived JWT** instead of the long-lived `RB_AGENT_API_KEY` that agent sessions use. This bounds the blast radius of a runtime process that handles untrusted model output: a leaked or prompt-injected token can only read one tenant's data for a few minutes.

**ADR**: [ADR-013 &sect;5](decisions/ADR-013-chat-panel-architecture.md) (Wave 9).
**Code**: `rb-auth::jwt` (mint/verify), `control-api routes/mcp/` (JWT auth path).

---

## Token shape

The MCP chat token is an HS256 JWT signed with `RB_MCP_JWT_SECRET` (stored in `rb-secrets`, rotatable via `kid` in the header).

```jsonc
{
  "iss": "control-api",
  "aud": "rb-mcp",
  "sub": "<chat_session_id>",        // UUID, session-scoped
  "tenant_id": "<uuid>",             // server-trusted tenant binding
  "user_id": "<uuid>",
  "scope": ["read"],                 // read-only MCP tools only
  "iat": 1717329600,
  "exp": 1717330500,                 // iat + RB_MCP_JWT_TTL_SECS (default 900 = 15 min)
  "jti": "<uuid>"                    // audit correlation + optional denylist
}
```

### Key properties

| Property | Value | Why |
|----------|-------|-----|
| Audience | `rb-mcp` | Prevents cross-service token confusion |
| Scope | `["read"]` | Only read tools (`search_items`, `get_item`, `get_callers`, `get_callees`, `get_trait_impls`); write/admin tools rejected with `-32601 insufficient_scope` |
| TTL | 15 min (default) | Caps the value window of a leaked token |
| Subject | session ID (not user ID) | One token per session; revoking a session implicitly invalidates the token |

---

## Token lifecycle

```
Session created
  │
  └─ chat-gateway mints JWT ──▶ written to .mcp.json (0600, isolated workspace)
     │
     ├─ runtime calls POST /mcp (Bearer <JWT>)
     │     └─ /mcp verifies signature + exp
     │        └─ AuthContext from claims (tenant_id, user_id, scope)
     │           └─ McpSessionStore binds tenant_id (drift rejection)
     │              └─ tools/call: scope enforcement + audit
     │
     ├─ token nearing expiry (within RB_MCP_JWT_REFRESH_SECS, default 120s)
     │     └─ chat-gateway re-mints JWT
     │        └─ rewritten to .mcp.json in the live workspace
     │
     └─ session ends / idle / crash
           └─ token left to expire (≤15 min)
              └─ optional: jti added to in-memory denylist on force-kill
```

The JWT exists in exactly **two places**: the chat-gateway's mint call (in-process memory) and the runtime's `.mcp.json` file (0600 permissions, isolated workspace). It is **never** placed in a prompt, a `chat_messages.body`, an `agent_events` payload, or a log line.

---

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `RB_MCP_JWT_SECRET` | — (**required**) | HS256 signing key. Store in `rb-secrets`. Rotation: set a new secret with a new `kid`; the verifier accepts both during a grace period. |
| `RB_MCP_JWT_TTL_SECS` | `900` (15 min) | Token time-to-live. Lower values reduce leak exposure; higher values reduce re-mint frequency. |
| `RB_MCP_JWT_REFRESH_SECS` | `120` (2 min) | Re-mint window. When the live token is within this many seconds of `exp`, the gateway mints a fresh one. |

---

## Verification path

The `/mcp` endpoint accepts two auth schemes:

1. **API key** (existing) — `Authorization: Bearer rb_...` — looks up `control.api_keys`.
2. **JWT** (Wave 9) — `Authorization: Bearer eyJ...` — verifies HS256 signature, checks `aud="rb-mcp"` and `exp`, then yields an `AuthContext` from the token claims.

The JWT path avoids a database round-trip per MCP call, keeping tool-call dispatch within the &le;200 ms p95 latency budget (ADR-013 &sect;8).

---

## Tenant binding and drift rejection

On the first `/mcp` request in a session, `McpSessionStore` binds the `Mcp-Session-Id` header to `claims.tenant_id`. Every subsequent `tools/call` checks that the request's `tenant_id` matches the bound value. A mismatch returns `TENANT_DRIFT (-32000)`. This reuses the existing `McpSessionStore` from Wave 7 — no new tenant-binding code.

The `tenant_id` is always server-fixed from the JWT claims, never from tool arguments. Even if a prompt-injected model attempts to pass a different tenant ID in tool args, the MCP server ignores it.

---

## Scope enforcement

| Scope | Permitted tools |
|-------|----------------|
| `read` | `search_items`, `get_item`, `get_callers`, `get_callees`, `get_trait_impls` |
| `admin` (not issued for chat) | `run_query` (raw Cypher) |
| `write` (future, not in Wave 9) | Mutating tools (ADR-013 &sect;9) |

A chat JWT carries `scope: ["read"]`. Any tool outside this scope is rejected with JSON-RPC error `-32601` and `data.reason="insufficient_scope"`.

---

## Audit

Every `tools/call` writes one row to `audit.audit_events` via the existing `write_tool_call_audit` function:

| Field | Value |
|-------|-------|
| `action` | `mcp.tools.call.<tool_name>` (e.g. `mcp.tools.call.search_items`) |
| `args` | SHA-256 hash of the tool arguments (never raw) |
| `actor_kind` | `mcp_client` |
| `outcome` | `success` or error code |
| `jti` | From the JWT claims — correlates chat session to audit trail |

No schema change to `audit.audit_events`. The `jti` field is added to the existing payload JSON column for chat sessions.

---

## Log-redaction contract

A single redaction pass is applied **before** any runtime output byte becomes:

- a `chat_messages.body`
- an `agent_events.data` field
- an SSE frame
- a structured log line

The redactor replaces matches with `<redacted:kind>`:

| Pattern | Kind | Example match |
|---------|------|---------------|
| Three base64url segments (`eyJ...\.…\.…`) | `jwt` | The MCP JWT itself |
| `(?i)bearer\s+[A-Za-z0-9._-]+` | `bearer` | `Bearer eyJ...` or `Bearer rb_live_...` |
| Exact live session token value | `session_token` | Verbatim echo of the JWT |
| `RB_MCP_JWT_SECRET` value | `secret` | The signing key |
| `RB_AGENT_API_KEY` / `rb_live_*` prefixes | `api_key` | Long-lived API keys |

### Fail-closed

If the redactor errors, the line is **dropped** and an `error_kind="redaction_failed"` event is emitted rather than persisting raw bytes. This is a safety invariant, not a performance optimization.

### Testing

QA (S6) must include a test that feeds a transcript line containing the live JWT and asserts it never reaches `chat_messages`, the SSE stream, or logs. See ADR-013 &sect;6.3.
