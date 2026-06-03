# ADR-013: Chat-Panel Architecture, Runtime Contract & MCP Token Model (Wave 9)

**Status:** Proposed (Gate-1 board review pending)
**Wave:** 9 (Phase -- Interactive Chat Panel)
**Author:** Architect ([RUSAA-1811](https://github.com/f-crop/rustacean/issues/641))
**Epic:** RUSAA-1810
**Covers streams:** S1 (this ADR), and the contracts consumed by S2 (runtime adapter), S3 (chat gateway + migration), S4 (frontend), S5 (MCP token + audit + redaction), S6 (QA), S7 (docs).
**Builds on:** [ADR-009](ADR-009-agent-execution-architecture.md) (agent execution: `rb-mcp`, `RuntimeAdapter`, `agent_sessions`/`agent_events`, SSE event relay, audit plumbing).
**Out of scope:** UI design specifics (S4 owns); runtime CLI installation/packaging (S2 owns); **mutating MCP tools** and their approval model (see &sect;9); multi-agent chat / agent-to-agent messaging; cross-tenant or shared chat memory; browser-side model inference.

---

## 1. Context

Wave 7 (ADR-009) shipped an **agent execution** surface: a `/mcp` Streamable-HTTP server (`rb-mcp` + `control-api routes/mcp/`), durable `agent_sessions`/`agent_events`, an SSE event relay, and a runtime-adapter substrate in `agent-runner` (`RuntimeAdapter` trait, `claude_code`/`opencode`/`pi` subprocess adapters, isolated workspaces, `kill_on_drop`, workspace GC). Wave 9 turns that substrate into an **interactive chat panel**: a logged-in user opens a chat, types a message, and watches a coding runtime answer -- using the same MCP read tools the agent surface already exposes.

### What is reused unchanged

| Already on disk | Where | Reused in Wave 9 for |
|---|---|---|
| `RuntimeAdapter` trait (`spawn`/`send_input`/`terminate`/`parse_stdout_line`), `SessionCtx`, `AgentProcess` (`kill_on_drop(true)`) | `services/agent-runner/src/adapters/mod.rs` | the runtime adapter contract (&sect;4) -- generalized, not re-invented |
| Subprocess supervision + isolated workspace + `workspace_gc` orphan reaper | `services/agent-runner/src/session/`, `workspace_gc.rs` | the process supervisor (&sect;4) |
| `McpSessionStore` -- binds `tenant_id` at `initialize`, rejects `tools/call` tenant drift with `TENANT_DRIFT (-32000)` | `crates/rb-mcp/src/session.rs` | chat MCP tenant binding (&sect;5) -- reused verbatim |
| `write_tool_call_audit` to `audit.audit_events` | `services/control-api/src/routes/mcp/audit.rs` | per-tool-call audit (&sect;5/&sect;6) -- reused verbatim |
| `.mcp.json` writer: 0600 perms, `http(s)`-scheme SSRF guard | `services/agent-runner/src/adapters/mod.rs::write_mcp_config` | runtime MCP-config injection (&sect;5) -- extended to carry a JWT |
| 6 read-only MCP tools (`search_items`, `get_item`, `get_callers`, `get_callees`, `get_trait_impls`, `run_query`) | `services/control-api/src/routes/mcp/`, `rb-query` | chat tool surface -- read-only subset (&sect;5/&sect;9) |
| SSE event relay + `Last-Event-ID` replay; `agent_events` append-only/partitioned pattern | `crates/rb-sse`, `agent-runner/src/event_relay/`, `migrations/control/` | chat token/message streaming (&sect;7) |
| Auth middleware (`Session{verified}` / `ApiKey{Read/Write/Admin}` extractor) | `services/control-api/src/middleware/auth.rs` | chat-gateway auth (&sect;3) + MCP JWT auth path (&sect;5) |

---

## 2. Decision Summary

| Area | Decision | Rationale |
|------|----------|-----------|
| **Topology** | Reuse the Wave-7 path. `control-api routes/chat/` (chat-gateway) creates a chat session, dispatches a runtime command over the existing Kafka command topic to `agent-runner`. No new binary, no new Kafka topic. | Every protocol surface lives in `control-api`/`agent-runner` since Wave 4. |
| **Runtime adapter (&sect;4)** | Generalize the existing `RuntimeAdapter` trait with `manifest()` + `health()`. A new runtime = one adapter impl + one registry entry. | The trait already exists; we widen it, not replace it. |
| **MCP token model (&sect;5)** | Replace the long-lived `RB_AGENT_API_KEY` with a short-lived JWT per chat session: `aud="rb-mcp"`, `scope:["read"]`, 15 min TTL. | Read-scoped, tenant-bound, minutes-TTL credential bounds the blast radius. |
| **Persistence (&sect;7)** | Two new tables: `chat_sessions`, `chat_messages`. Migration `021_chat_panel.sql`, additive only. | Mirrors `agent_sessions`/`agent_events` precedent. |
| **Latency (&sect;8)** | First-token (warm): &le;1.5 s p95. Tool-call dispatch: &le;200 ms p95. Cold start: &le;4 s p95, tracked separately. | Warm first-token and server-side tool-dispatch are the two figures we actually control. |
| **Flag-gating** | Entire chat panel behind `RB_CHAT_PANEL_ENABLED` (default off) via `rb-feature-resolver`. | Lets S2--S5 land incrementally on `main` without exposing an unfinished surface. |

---

## 3. Component map

Five components, all inside Rustacean:

```
   Browser
   +------------------------------+
   | (1) ChatPage UI  [S4]        |  flag-gated; TanStack Router + React Query
   |   - message composer         |  + useEventSource (REQ-FE-08 hook)
   |   - streaming transcript     |
   +-----------+------------------+
               | POST /v1/chat/sessions ; POST /v1/chat/sessions/{id}/messages
               | GET  /v1/chat/sessions/{id}/events   (SSE)
   +-----------v------------------+
   | (2) chat-gateway  [S3]       |  control-api  routes/chat/
   |   - create/list/get/end      |  - auth: verified session OR API-key(agent|admin)
   |   - mint MCP JWT  -----------+------------+  (5) MCP token model [S5]
   |   - persist sessions/msgs    |            |  rb-auth::jwt  (mint/verify)
   |   - dispatch runtime cmd     |            v  aud=rb-mcp, scope=[read], short exp
   +-------+---------------+------+   +------------------------+
           |               |          | control-api routes/mcp/| JWT auth path (NEW)
   Kafka: rb.agent.commands|          | McpSessionStore        | (reused)
           |               |          | write_tool_call_audit  | (reused)
   +-------v---------------v------+   +----------^-------------+
   | (3) runtime adapter  [S2]    |              | POST /mcp  (Bearer <JWT>)
   | agent-runner supervisor      |              |  read tools only
   |  - RuntimeAdapter (general)  |--------------+
   |  - 1 process / session       |
   |  - isolated workspace + .mcp |  .mcp.json carries the JWT (0600)
   |  - stdio bridge -> event relay
   +-------+----------------------+
           | stdout (redacted) -> agent_events relay -> SSE
   +-------v----------------------+
   | (4) persistence  [S3]        |  control.chat_sessions / control.chat_messages
   |  migration 021 (additive)    |  body = post-redaction text only
   +------------------------------+
```

---

## 4. Runtime adapter contract

See [runtime-adapter.md](../runtime-adapter.md) for the full developer-facing contract and authoring guide.

The trait is widened with `manifest()` and `health()`. Existing adapters (`claude_code`, `opencode`, `pi`) compile against the wider trait with trivial fill-ins.

---

## 5. MCP token model

See [mcp-chat-tokens.md](../mcp-chat-tokens.md) for the full token specification, lifecycle, and redaction contract.

HS256, signed with `RB_MCP_JWT_SECRET`, claims `{sid, tenant_id, user_id, scope:["read"], exp}`. 15 min TTL, re-minted on activity.

### 5.1 Key management

**Entropy requirement.** `RB_MCP_JWT_SECRET` must be a cryptographically random value of ≥ 256 bits (32 bytes), loaded exclusively via `rb-secrets::from_env("RB_MCP_JWT_SECRET")`. Inline literals, `.env` commits, and plaintext database columns are prohibited.

**Distinctness.** `RB_MCP_JWT_SECRET` must be distinct from every other signing key in the system — `RB_GITHUB_APP_PRIVATE_KEY`, session-token secrets, and all future keys. Sharing a key across authentication boundaries is prohibited.

**Rotation procedure.** The `kid` (key ID) header in each JWT (see token shape above in §5) is the rotation hook:

1. Generate a new random value; assign it `kid = N+1`.
2. Deploy the new value **alongside** the old one. The verifier (`rb-auth::jwt::verify`) must support **at least two simultaneous `kid` values** during the overlap window; tokens signed by either key must verify successfully.
3. The overlap window equals `RB_MCP_JWT_TTL_SECS` (default 900 s) — the maximum in-flight JWT lifetime. Once the window passes, no token signed with the old key remains valid.
4. Remove the old value. The verifier reverts to accepting only the current `kid`.

**Operational note.** Secrets loaded via `rb-secrets::from_env` are read at mint time. A forced session kill is not required: all old-key tokens expire naturally within one TTL window.

---

## 6. Threat model (Security-Engineer-owned)

| # | Threat | Mitigation |
|---|--------|-----------|
| T1 | Prompt-injection reads another tenant | `tenant_id` server-fixed from JWT; `McpSessionStore` drift rejection; read-only scope |
| T2 | JWT leaks into transcript | Log-redaction contract + token never in prompts; read-scoped + &le;15 min TTL |
| T3 | Compromised process reads host FS | Isolated workspace, no shared mount, no host secret; only the read-scoped JWT |
| T4 | Token replay after session end | Short `exp`; `jti` denylist on force-kill |
| T5 | SSRF via MCP URL | Existing `http(s)`-scheme guard |
| T6 | Tenant switch mid-session | `McpSessionStore` drift rejection |
| T7 | Resource exhaustion / fork-bomb | cgroup caps, concurrency limits, wall-clock + idle caps |
| T8 | Weak or shared JWT signing key | `RB_MCP_JWT_SECRET` ≥ 256-bit random, distinct from all other keys, loaded via `rb-secrets::from_env`; rotation per §5.1 |

---

## 7. Persistence schema

Migration `021_chat_panel.sql` (additive only). Two new tables in `control`:

- `chat_sessions` -- id, tenant_id, user_id, runtime, status, trace_id, created_at, last_activity_at, ended_at. `tenant_id` FK `ON DELETE CASCADE` + immutability trigger.
- `chat_messages` -- id, session_id, tenant_id, seq, role, body (post-redaction), created_at. Unique on `(session_id, seq)`.

Retention: 90-day terminal-session purge (cron, 02:00 UTC).

---

## 8. Latency budget

| Metric | Budget | Boundary |
|--------|--------|----------|
| First-token (warm) | &le;1.5 s p95 | gateway receipt to first SSE assistant token (process already live) |
| Tool-call dispatch | &le;200 ms p95 | MCP `tools/call` receipt to `CallToolResult` (server-side) |
| Cold start | &le;4 s p95 | session create to process ready (tracked, not part of first-token SLO) |

---

## 9. Forward-looking: mutating tools

All chat tools in Wave 9 are **read-only**. Mutating tools require a future ADR covering a human-in-the-loop approval gate, `write` token scope, write-path audit, and an expanded threat model.

---

## 11. Rejected alternatives

- New `services/chat-gateway` binary -- duplicates auth/OTel/audit/SSE plumbing.
- Runtime inside `control-api` (skip Kafka) -- forks process supervision.
- Reuse `RB_AGENT_API_KEY` for chat -- wrong blast radius for untrusted model output.
- Opaque token instead of JWT -- extra DB round-trip per `/mcp` call.
- WebSocket transport -- SSE already hardened with `Last-Event-ID` replay.
- Per-message process spawn -- blows the first-token budget.
