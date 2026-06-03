# Changelog

All notable changes to Rustacean (rust-brain) are documented in this file. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

---

## [Wave 9] - 2026-06-xx (Unreleased)

Interactive chat panel + pluggable runtime over Rustacean MCP.

### Added

- **Chat panel** -- interactive coding assistant in the UI. Users open a chat session, send messages, and stream assistant responses in real time via SSE. Flag-gated behind `RB_CHAT_PANEL_ENABLED` (default off).
- **Chat gateway** -- `POST /v1/chat/sessions`, `POST /v1/chat/sessions/{id}/messages`, `GET /v1/chat/sessions/{id}/events` (SSE), `POST /v1/chat/sessions/{id}/end`. Authenticated via session cookie or API key.
- **Runtime adapter generalization** -- `RuntimeAdapter` trait widened with `manifest()` and `health()` methods. A new runtime plugs in with one adapter impl + one registry line; zero gateway/persistence changes.
- **MCP chat tokens** -- short-lived HS256 JWT (`aud=rb-mcp`, `scope=["read"]`, 15 min TTL) replaces the long-lived `RB_AGENT_API_KEY` for chat sessions. Minted per session, auto-refreshed on activity, tenant-bound.
- **Log-redaction contract** -- mandatory redaction pass strips JWTs, bearer tokens, and secrets from runtime output before persistence, SSE, or logging. Fail-closed: unredactable lines are dropped.
- **Chat persistence** -- `control.chat_sessions` and `control.chat_messages` tables (migration `021_chat_panel.sql`). 90-day retention, `ON DELETE CASCADE` to tenants.
- **Scope enforcement** -- chat MCP tools restricted to read-only (`search_items`, `get_item`, `get_callers`, `get_callees`, `get_trait_impls`). Write/admin tools rejected with `-32601 insufficient_scope`.

### Documentation

- `docs/chat-panel.md` -- user guide (enable flag, prompt examples, troubleshooting)
- `docs/runtime-adapter.md` -- runtime adapter contract and new-adapter authoring guide
- `docs/mcp-chat-tokens.md` -- MCP token model, lifecycle, redaction contract
- `docs/decisions/ADR-013-chat-panel-architecture.md` -- architecture decision record
- `docs/runbook.md` -- Wave 9 operator section (flag toggle, key rotation, runtime inspection)

### Architecture

- ADR-013: Chat-Panel Architecture, Runtime Contract & MCP Token Model
- No new binary, no new Kafka topic. Chat reuses the Wave 7 agent execution substrate (`agent-runner`, SSE relay, `McpSessionStore`, audit).
- JWT mint/verify in `rb-auth` (pure crate, no service dep). Preserves `crates <- services` one-way rule.

---

## [Wave 8] - 2026-05-31

Hardening & polish across all services.

### Added

- Admin v1 operator endpoints (bootstrap, impersonate, force-delete, rebind-gh-install, audit-log)
- Grafana dashboards + `grafana-lint` CI gate
- Distributed tracing: `X-Trace-Id` header + trace-redirect endpoint
- `GET /metrics` on all services (Prometheus scrape)
- Synthetic-load harness for 7-day pre-prod soak
- Drift detector promoted to fail-on-drift + scheduled GHA
- Board happy-path E2E smoke suite

### Documentation

- ADR-012: Wave 8 Hardening & Polish

---

## [Wave 7] - 2026-05-19

Agent execution architecture.

### Added

- MCP server (`POST /mcp`) with read-only tools
- Agent sessions (`/v1/agents/sessions/*`) with streaming SSE events
- Runtime adapters: Claude Code (OAuth), OpenCode (LiteLLM), Pi (stub)
- Workspace isolation + `workspace_gc` orphan reaper
- SSE event relay with `Last-Event-ID` replay

### Documentation

- ADR-009: Agent Execution Architecture (rev 6)
- Operator runbooks bundle
