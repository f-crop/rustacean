# ADR-009: Agent Execution Architecture (Wave 7 — Phase 6)

**Revision:** rev 5 (2026-05-07) — RUSAA-895 pi-runtime evaluation: NEW §3.5 documents the pi binary identity (`@mariozechner/pi-coding-agent`), authentication / input / output properties, the absence of MCP client support, and the host-tool sandbox concern; recommends **closing the pi slot for Wave 7** (drop from `runtime_kind` enum, remove `PiRuntime` from `crates/rb-agent-runtime`, restrict §6.4 dispatch to two runtimes). §15 updated: pi-evaluation moved from open question to RESOLVED with pointer to §3.5; the "additional runtime adapters" bullet now lists `pi_local` as a re-opening candidate alongside `codex_local`, etc. Subject to Gate-1 re-approval — touches frozen items (`runtime_kind` CHECK constraint in §4.1, dispatch table in §6.4).

**Revision:** rev 4 (2026-05-07) — RUSAA-859 security fix: §4.1 `input_prompt` replaced with `input_prompt_preview` (≤256 chars, never stores PII/credentials verbatim); §4.1 adds 90-day retention purge function. Migration 011 implements the change; frozen-plan clause does not cover security-mandated schema corrections.

**Revision:** rev 3 (2026-05-07) — incorporates the board LLM-provider directive (CTO comment `ce3d7c11`, 2026-05-06; reproduces board decision on §11.1):
1. §11.1 closed — moved from open question to a **decided** architecture section; pointer added to the new topology in §3.4 / §6.4 / §7.5 / §8.
2. §1 — new substrate gap row for `crates/rb-agent-runtime` (runtime adapter abstraction) and the **LiteLLM gateway** dependency.
3. §2 — Decision Summary rows refreshed: NEW row "LLM runtime topology" (LiteLLM gateway + three runtimes — Claude Code via user OAuth, OpenCode + Pi via LiteLLM); previous "No new infra" row qualified to acknowledge LiteLLM as the one new external service.
4. §3.4 — NEW: LiteLLM placement (shared in-cluster service vs sidecar) + runtime adapter shape inside `control-api` Phase 1, `services/agent-runtime` Phase 2.
5. §6.4 — NEW: per-runtime dispatch contract (`runtime_kind` field on session create; `runtime_kind` → adapter mapping; per-runtime auth resolution).
6. §7.5 — NEW: OAuth token storage for Claude Code (per-user, encrypted at rest, refresh handling) + LiteLLM virtual-key scoping per tenant (one LiteLLM "team" per Rustacean tenant; budget mirrored).
7. §8.1 — env-var table refactored: `AGENT_LLM_PROVIDER` removed (single-provider assumption invalidated); replaced with `LITELLM_*`, `AGENT_RUNTIMES_ENABLED`, `OAUTH_CLAUDE_*`, `AGENT_DEFAULT_RUNTIME`, `AGENT_DEFAULT_MODEL_BY_RUNTIME`; §8.2 adds runtime + LiteLLM metrics; §8.3 adds `LiteLLMUnreachable` and `OAuthClaudeRefreshFailureRate` alerts.
8. §15 / §16 — out-of-scope refreshed; new self-grill row covering "why LiteLLM rather than direct provider SDKs".

Rev 2 (2026-05-07) — closes three non-blocking items from CTO technical review (comment `77478af8`, 2026-05-06): §4.1 trigger DDL, §4.2 `rb_agent_writer` role, §8.3 `AgentEventsPartitionLag` alert.

Rev 1 (2026-05-07) — initial draft for Gate-1 review.

**Status:** Proposed (awaiting board sign-off on rev 5 — RUSAA-895 pi-slot deferral touches frozen items §4.1 / §6.4; CTO technical review **APPROVE** rev 1, 2026-05-06; rev 3 supersedes the rev 1/2 single-provider working assumption per board directive 2026-05-06; rev 5 narrows rev 3's three-runtime set to two for Wave 7 — see §3.5)
**Wave:** 7 (Phase 6 — Agent Execution)
**Author:** Architect ([RUSAA-719](/RUSAA/issues/RUSAA-719))
**Covers requirements:** REQ-MC-01 (RUSAA-83), REQ-MC-02 (RUSAA-84), REQ-FE-06 (RUSAA-85)
**Builds on:** [ADR-006](/RUSAA/issues/RUSAA-293#document-plan) (transport surface — `rb-kafka`, `rb-blob`, `rb-sse`), [ADR-007](/RUSAA/issues/RUSAA-382#document-plan) (Wave 5 ingestion + semantic graph), [ADR-008](/RUSAA/issues/RUSAA-467#document-plan) (Wave 6 data-plane queries + `rb-query`)
**Wave plan parent:** epic `e9d83e51-4f88-4f07-a2d0-fe1ab8051ae8` (Phase 6 — Agent Execution; children RUSAA-83, RUSAA-84, RUSAA-85)
**Out of scope:** Multi-agent orchestration / agent-to-agent messaging (Phase 7+); fine-tuning or model training; non-MCP tool protocols (OpenAI tools, function calling); agent-authored code commits / write-back to repos (Phase 8 — needs separate trust model); shared / cross-tenant agent memory (single-tenant per session); browser-side LLM inference (all model calls server-side); a separate `agent-runtime` binary (deferred — see §3, §15); MCP **client** library (only server surface ships in Wave 7); **self-hosting LiteLLM customisations** beyond config — we run upstream LiteLLM with our config; our own fork is out-of-scope; **operating Claude Max plan provisioning for tenants** — each tenant brings their own Claude OAuth identity; we host the OAuth flow but not the subscription.

---

## 1. Context

Wave 6 (ADR-008, RUSAA-15) shipped a **read-only data plane** — `crates/rb-query` plus 11 control-api endpoints — that resolves item lookup, caller/callee traversal, trait impls, semantic search, raw Cypher, and module trees against the per-tenant projections produced by Wave 5. The data-plane API is the substrate every agent tool will call into.

Wave 7 (this ADR) turns that substrate into an **agent execution surface**:

1. An MCP (Model Context Protocol) **server** that exposes 6 read-only code-intelligence tools (REQ-MC-01).
2. A backend **agent execution session** model with persistent runs, live event streams, and trace pinning (REQ-MC-02).
3. A frontend **agent execution viewer** — start a run, watch tool calls live, replay session history (REQ-FE-06).

What is **already on disk and reused unchanged** (verified 2026-05-07 against `main`):

| Already present | Where | Used in Wave 7 by |
|-----------------|-------|-------------------|
| `crates/rb-query` (semantic search, item lookup, callers/callees, trait impls, raw Cypher, module tree) | `crates/rb-query/src/{semantic,pg,graph,lib}.rs` | every MCP tool wraps a `rb_query` function — zero new data access code |
| `rb-sse` `EventBus` + 5-min reconnect ring buffer + `Last-Event-ID` replay | `crates/rb-sse/` | live agent event stream (REQ-MC-02 SSE endpoint, REQ-FE-06 UI) |
| `rb-tracing` OTLP exporter + W3C `TraceContext` propagator + `current_trace_id()` helper | `crates/rb-tracing/src/lib.rs` | every agent session is one root OTel span; trace_id pinned on session row |
| Auth middleware: `Session{verified, expired}` / `ApiKey{Read|Write|Admin}` extractor | `services/control-api/src/middleware/auth.rs` | every Wave 7 endpoint reuses this (extended with one new scope — §7.2) |
| `audit.audit_events` (append-only, INSERT-only role) | `migrations/control/006_audit_events.sql` | every tool call written here for tenant-leak proof + compliance log |
| `control` schema migration runner + `migrate` binary | `services/migrate/`, `migrations/control/*.sql` | adds two new tables (§4.1 / §4.2) |
| OpenAPI generation (`utoipa`) + `cargo run -p control-api -- print-openapi` + `openapi-typescript` to `frontend/src/api/generated/schema.ts` | `services/control-api/src/openapi.rs` + `scripts/check-openapi-sync.sh` | every Wave 7 REST endpoint annotated; CI sync check enforced |
| Frontend conventions: TanStack Router, React Query, shadcn/ui, generated API hooks | `frontend/src/api/hooks`, `frontend/src/components/`, `frontend/src/pages/` | `/agents/*` route pair (REQ-FE-06) |
| Tempo HTTP `GET /api/traces/{traceId}` + in-app trace viewer (REQ-FE-08, ADR-008 §3.x) | running deployment + `frontend/src/pages/TraceViewer.tsx` (Wave 6 deliverable) | each agent session links to `/trace/$traceId` |

Non-negotiables (PRD + COMPANY.md) that govern this wave:

- **Architectural laws:** no `services/*` deps in `crates/rb-*`; ≤600-line files; one binary per Kafka consumer (Wave 7 adds zero consumers); all Neo4j reads through `TenantGraph::run`; all PG reads through `TenantPool`; all Qdrant reads through `crates/rb-storage-qdrant` wrapper.
- **Tenant isolation invariant:** the agent's tenant_id is taken from the authenticated `AuthContext` and is **never** read from request bodies, MCP tool arguments, or session state passed by the agent. It is server-fixed at session-creation time.
- **Frozen-plan rule (COMPANY.md Gate 1):** once approved, the MCP tool surface, the session/event table contracts (§4), the SSE envelope schema (§5), and the API-key scope additions (§7.2) are immutable until a re-Gate-1 revision; cosmetic edits exempt.
- **Vertical-slice rule:** Wave 7 ships a tracer-bullet (REQ-MC-01 *minimal* — `search_items` + `get_item` only — plus REQ-MC-02 session create/event SSE plus REQ-FE-06 first slice — start session, see streaming events) end-to-end before the rest land. See §9.
- **No new binaries:** Phase 1 extends `control-api`. A Phase-2 split into `services/agent-runtime` is described in §3 with explicit trigger metrics; we do not split today.

Pre-existing **leftovers / open scars** Wave 7 must absorb (not invent — Wave-6 leftovers):

- **Leftover-1 (`code_embeddings` pgvector still empty).** ADR-008 §15 deferred removal. Wave 7 reads only Qdrant via `rb-query`; the empty pgvector table remains untouched. Tracked in §15.
- **Leftover-2 (Tempo unauthenticated).** Tempo's HTTP API is currently exposed without auth on the cluster-internal network. Agent trace links from REQ-FE-06 surface go through the same browser-side fetch as REQ-FE-08; if Tempo auth is added in a future wave, both viewers update together. Documented context, not a Wave-7 defect.

Substrate gaps Wave 7 introduces (two new crates, zero new Rustacean binaries; one new external dependency):

- **`crates/rb-mcp`** — MCP protocol library. Pure library crate (no binary). Owns the JSON-RPC envelope, the tool-registration schema, the Streamable HTTP transport handler, and the OAuth/Bearer auth bridge. Consumed by `services/control-api` to expose `/mcp` HTTP endpoints. **No reverse dependency** — `rb-mcp` does NOT depend on `rb-query` (handlers wire MCP tool calls into `rb-query` calls inside `control-api`).
- **`crates/rb-agent-runtime`** — runtime adapter abstraction. Defines the `AgentRuntime` trait (start session, stream events, cancel) plus two implementors: `ClaudeCodeRuntime` (drives Anthropic Messages API via the user's OAuth token; mirrors Paperclip's `claude_local` adapter shape) and `OpenCodeRuntime` (drives OpenCode through LiteLLM; mirrors `opencode_local`). Pi was evaluated and deferred — see §3.5. Pure library crate; consumed by `services/control-api` (Phase 1) and movable wholesale to `services/agent-runtime` (Phase 2 — §3.2). Each adapter is a thin wrapper: HTTP client, request shaping, response parsing, token / cost accounting. **No reverse dependency** on `rb-query`; tool calls reach back via callbacks supplied by the host process.
- **External dependency: LiteLLM** — upstream open-source proxy ([`BerriAI/litellm`](https://github.com/BerriAI/litellm)). Runs as one in-cluster service (default replica = 2 for HA) fronting all non-OAuth model calls. Owns: provider abstraction (Anthropic, OpenAI, Bedrock, Vertex, local-Ollama), per-tenant virtual-key issuance, per-tenant budget caps, request/response logging hooks. Rustacean depends on the **LiteLLM HTTP API surface** (OpenAI-compatible Completions / Messages endpoints + virtual-key admin REST). We do NOT fork or vendor LiteLLM source. Wave 7 adds: helm chart values, virtual-key provisioning script (one key per tenant, scoped by `tenant_id` metadata), and the dial-in for the one LiteLLM-routed runtime (OpenCode).
- **`services/control-api`** — gains:
  - one new module `routes/agents/` (sessions, events, tool calls)
  - one new module `routes/mcp/` (MCP Streamable HTTP entry point + tool dispatch)
  - one new module `routes/auth/oauth/claude/` (OAuth 2.0 PKCE callback handler for Claude Code's user-Max-plan token flow — start, callback, refresh-store; details §7.5)
  - one new persistent component: per-process `AgentRegistry` (in-memory map of active sessions) plus the durable `agent_sessions` / `agent_events` / `oauth_tokens` tables (the third is NEW — §4.4)
  - one new event flavour on `EventBus`: `AgentEvent` (typed, see §5).

---

## 2. Decision Summary

| Area | Decision | Rationale |
|------|----------|-----------|
| **MCP transport** | **Streamable HTTP** (current MCP spec, March 2025); single `POST /mcp` endpoint accepts JSON-RPC requests; long-lived responses use the same connection with chunked SSE inside the HTTP body when a tool returns streaming output. **No stdio transport** (browser/agent clients don't need it; CLI users can drive via `curl` or future `rb-mcp-cli`). | Streamable HTTP is the protocol's blessed transport for hosted servers; matches our existing axum routing; reuses our `Authorization: Bearer` API-key path; gives us one URL to authenticate, audit, and rate-limit. SSE-inside-HTTP is part of the spec, not a deviation. |
| **MCP topology** | **Extend `control-api`** with `routes/mcp/` module; do NOT introduce a new binary. Tools dispatch into `rb-query` directly. | Wave-1..6 precedent: every protocol surface (REST, SSE) has lived in `control-api`. MCP traffic in Phase 1 is single-digit concurrent sessions per tenant (§3 capacity model); a new binary buys nothing today and would duplicate auth/OTel/audit/openapi plumbing. The Phase-2 split trigger is explicit (§3). |
| **Tool surface (REQ-MC-01)** | 6 tools, all read-only, all wrappers over `rb-query`: `search_items`, `get_item`, `get_callers`, `get_callees`, `get_trait_impls`, `run_query`. Each tool has a JSON schema published via `tools/list` JSON-RPC. | One-to-one mapping with the data-plane endpoints from ADR-008 §3.1–3.5. Read-only because we have no agent-write trust model yet (out of scope, §15). |
| **Session model (REQ-MC-02)** | A **session** = one agent run, scoped to one (tenant, user, agent_id). Persisted in `control.agent_sessions`; events appended to `control.agent_events`. Lifecycle: `pending → running → completed / failed / canceled`. Idle timeout 15min; hard cap 60min wall-clock. | Sessions are the unit of audit, billing, and trace scope. Append-only event log is the durable record (replay any session by selecting `agent_events WHERE session_id`). |
| **Tenant isolation** | `agent_sessions.tenant_id` is set from `AuthContext.tenant_id` at session-creation time and is **immutable** for the session's lifetime. Every tool call inside the session passes that fixed tenant_id to `rb-query`; the agent cannot supply one. The tool-call handler **rejects** a session if the caller's current `AuthContext.tenant_id` ever drifts from the row (e.g. user switched tenants mid-session). | Closes the threat model where a malicious agent prompt convinces the LLM to set `tenant_id` in tool args; we never read it from args. Mirrors ADR-008 's "all reads scoped via TenantPool / TenantGraph / Qdrant filter" — we just push the chokepoint up one layer to session creation. |
| **Event streaming (REQ-MC-02)** | **SSE via `rb-sse`** (already in production). NEW topic family `agent.events` on the per-tenant broadcaster. Event shape — `{type, session_id, ts, data}` — see §5. NEW endpoint `GET /v1/agents/sessions/{id}/events` (auth: same session that created the agent, OR API-key with `agent` scope). Reconnect via existing `Last-Event-ID` ring buffer (5min). | WebSocket rejected: agent events are unidirectional (server → client); SSE is already production-hardened with reconnect + auth + 6-conn HTTP/2 multiplexing. Adding a WebSocket library would be net-new infra for zero functional gain. |
| **Trace capture (REQ-MC-02)** | Each session opens a root span `agent.session.run` carrying `tenant.id`, `agent.id`, `session.id`, `user.id`. Tool calls are child spans `agent.tool.<name>` with `tool.args.size`, `tool.duration_ms`, `tool.result.size`. LLM calls are child spans `agent.llm.call` with `llm.model`, `llm.input_tokens`, `llm.output_tokens`, `llm.latency_ms`, `llm.cost_usd`. The 32-hex `trace_id` is pinned on `agent_sessions.trace_id` at session start. | Reuses `rb-tracing::current_trace_id()` (already wired to OTLP → Tempo). Pinning the trace_id on the session row means REQ-FE-06 can render a `/trace/$traceId` link without re-deriving it. Standard span naming follows OTel GenAI semantic conventions where they exist. |
| **Security model** | (1) NEW API-key scope **`agent`** — narrower than `admin`, broader than `read`/`write`. Required to start a session; required to call MCP tools. Sessions started via browser cookie auth automatically inherit the user's tenant + role. (2) Per-session **token budget** — default 100 000 input / 20 000 output tokens; configurable per request; hard-fail at budget. (3) Per-tenant **rate limit** — 10 sessions / minute, 100 active sessions max (tunable via env). (4) Every tool call written to `audit.audit_events` with `(session_id, tool_name, args_hash, result_status)`. | API-key scope additions are the cheapest correct way to keep agent traffic separable from data-plane reads (auditable, revocable independently). Token budget is the budget cap that keeps prompt-injection blowups bounded. Audit log is the cross-tenant-leak proof, same pattern as ADR-006/007/008. |
| **Frontend (REQ-FE-06)** | NEW route pair: `/agents` (list / start a session) and `/agents/$sessionId` (live session viewer). Three-pane layout: **left** session history list, **center** streaming event log (chronological, virtualized), **right** session metadata + trace_id link + cancel button. Reuses TanStack Router, React Query, the existing API hooks pattern, and shadcn/ui. | Three-pane is the same shape as the Code Intelligence Workspace (REQ-FE-05) so users encounter one consistent layout idiom. Live event log uses the existing `useEventSource` hook from REQ-FE-08. |
| **LLM runtime topology** | **LiteLLM gateway + two Rustacean agent runtimes** — `claude_code` (drives Anthropic Messages API via the **user's** OAuth token from their own Claude Max plan; **no API-key spend on Rustacean's books**) and `opencode` (drives OpenCode through LiteLLM). LiteLLM owns provider abstraction, virtual-key issuance per tenant, budget caps, and audit logging hooks for the one API-keyed runtime. Mirrors the first two slots of Paperclip's multi-runtime adapter model (`claude_local`, `opencode_local`); the `pi_local` slot was evaluated and deferred — see §3.5. New session field `runtime_kind` selects the adapter (§6.4). | Three deliberate consequences: (1) Claude Code spend is the user's, removing the "single-tenant prompt-injection blows our LLM budget" failure mode for that runtime; (2) the LiteLLM-routed runtime gets a consistent provider story (any future model add is a LiteLLM config change, not Rustacean code); (3) we mirror Paperclip's runtime taxonomy so internal users have one mental model across products. The cost is one new external service in the cluster (§3.4) and an OAuth-token storage table (§4.4). |
| **No new infra (qualified)** | Zero new Kafka topics. Zero new Postgres schemas (three new tables in `control` — `agent_sessions`, `agent_events`, `oauth_tokens`). Zero new Rustacean binaries. **One** new external service: **LiteLLM** (upstream OSS, in-cluster, two replicas; §3.4). | The single new external service is justified by the runtime-topology decision above — it replaces what would otherwise be three direct provider SDK integrations (Anthropic, OpenAI, future Bedrock) with one. Net surface is *smaller*, not larger. |
| **Tracer-bullet** | RUSAA-83-min (`search_items` + `get_item` MCP tools only) + RUSAA-84-min (session create + SSE event stream) + RUSAA-85-first-slice (start a session via UI, see live events, no replay/cancel UI yet) ⇒ end-to-end demo where a developer says "find the type for `Vec::push`" and watches the agent's tool calls flow live in the UI. The remaining tools, full session lifecycle controls, and history/replay land after the tracer. | Same model as ADR-007 / ADR-008 tracer-bullets. Proves the MCP→tool→SSE→UI pipeline before fanning out. |
| **Owner split** | **Services Engineer** (Sonnet 4.6): RUSAA-83 + RUSAA-84 (backend — MCP server, agent sessions, event SSE). **Frontend Engineer**: RUSAA-85 (UI). **No Platform Engineer work** unless §11.1 lands a Phase-2 binary split. | Mirrors Wave 6's split. |
| **Open questions** | Three for the board; see §11. Each has a stated working assumption that lets the wave ship without the answer. | Same model as ADR-006/007/008. |

---

## 3. Service topology — extend vs. split

This is the load-bearing decision in this ADR; documenting the reasoning explicitly.

### 3.1 Phase 1 — extend `control-api`

We add three things to `control-api`:

1. `routes/mcp/` — MCP Streamable HTTP entry point. One axum handler, dispatches JSON-RPC method names to tool implementations.
2. `routes/agents/` — REST surface for agent sessions (list / create / get / cancel) plus the SSE event stream endpoint.
3. `state::AgentRegistry` — in-memory map `Uuid → ActiveSession`, holding the running task handle, token-budget meter, and event-publisher handle. Bounded to `MAX_ACTIVE_SESSIONS_PER_PROCESS` (default 200; backed by per-process semaphore).

Capacity model (rough but enough to gate the decision):

- Phase-1 expected concurrency: **≤ 50 active sessions per tenant**, **≤ 5 tenants concurrently active**, ⇒ ≤ 250 active sessions per region.
- Per-session resources: ~5 MB heap (tokio task + LLM client buffers + event ring), ~1 outbound HTTP/2 connection to the LLM provider.
- One `control-api` replica today carries ingestion-status SSE (Wave 4), data-plane reads (Wave 6), auth, GitHub webhooks. Adding agents at the Phase-1 numbers raises baseline RSS by ≤ 1.25 GB and adds at most 250 long-lived tokio tasks — well under the existing replica's 4 GB / 4 vCPU envelope.

What this **does not** buy us:

- An agent infinite loop **could** starve the control-api event loop. Mitigations: (a) tokio task per session (preemption already handled), (b) LLM call timeout (default 60s per call), (c) hard wall-clock cap at 60min per session, (d) `MAX_ACTIVE_SESSIONS_PER_PROCESS` semaphore so we degrade gracefully (new sessions return 429 rather than OOM).

### 3.2 Phase 2 — split into `services/agent-runtime` (deferred)

We split when **any** of the following is observed for two consecutive days:

- Control-api p95 request latency on non-agent routes > 200 ms (today's baseline ≈ 35 ms).
- Active sessions per replica > 100 sustained.
- Per-replica RSS > 3 GB sustained.
- LLM call timeouts > 1 % over a 6h window (proxies blocking).

The split shape is mechanical:

- Lift `routes/mcp/` + `routes/agents/` + `state::AgentRegistry` into a new binary `services/agent-runtime`, listening on its own port (default 3110).
- `control-api` keeps a thin proxy: `POST /v1/agents/sessions` and `GET /v1/agents/sessions/{id}/events` forward to `agent-runtime` over loopback HTTP/2 with the `Authorization` header passed through verbatim.
- The `agent_sessions` / `agent_events` tables stay in the `control` schema (single DB), so no migration is needed at split time.
- `EventBus` stays per-process; cross-process broadcast is solved by either (a) `agent-runtime` publishing to Kafka and `control-api` subscribing for the SSE proxy (matches Wave-4 pattern), or (b) running the SSE endpoint in `agent-runtime` and routing it through the reverse proxy. Decision deferred to the split moment.

Naming `crates/rb-mcp` (vs. `services/mcp-server`) on day one is the cheap part of the split: the protocol code is already in a crate that any binary can host.

### 3.3 Why not split today?

- **Cost of split today** = 1 new binary, 1 new Kubernetes service, 1 new health probe, 1 new config surface, 1 new release pipeline, 1 new metrics scrape target, 1 new audit boundary in CI. (Each adds review-and-debug load on Platform Engineer / SRE, but neither is staffed for Wave 7.)
- **Benefit of split today** = isolation that the capacity model says we don't need yet.
- **Cost of split later** = the §3.2 list, but on a known load curve with real metrics, not speculation.

The decision asymmetry says: **defer**. Document the trigger so we don't drift past it silently.

### 3.4 LiteLLM placement

LiteLLM is the one new external service in Wave 7. Two placement shapes were considered:

**Option A — Sidecar (one LiteLLM container per `control-api` pod).** Loopback HTTP, lowest latency, replicated cost.

**Option B — Shared in-cluster service (one LiteLLM Deployment, two replicas, ClusterIP).** Single control surface for budgets and virtual keys; one LiteLLM upgrade affects all replicas; cross-pod HTTP adds ≤2 ms.

**Decision: Option B (shared in-cluster service).** Rationale:

- **Single control surface for tenant budgets / virtual keys** — LiteLLM tracks `tenant_id` budget on the *gateway*, not per pod. Sidecars would require either each sidecar replicating tenant state from a central store on boot, or a write-through cache; both are net-new infrastructure. A shared service has tenant state in one place.
- **Independent scaling** — Wave 7's session-create rate is bursty; LiteLLM CPU is dominated by JSON encoding + provider HTTP calls. We want LiteLLM to scale on its own metrics (request rate, provider connection pool saturation), not on `control-api` HPA signals.
- **Audit / log centralisation** — LiteLLM's request logs go to one Loki stream, not N. Per-tenant cost rollups query one source.
- **Operational reversibility** — moving from a shared service to sidecars later is a Helm-values flip; the reverse direction is harder once tenant state is in the sidecars' local memory. Pick the lower-coupling shape today.

**Topology details:**

- Helm chart: `charts/litellm/` (vendored from upstream chart values + our `litellm.yaml` config).
- ServiceAccount: `litellm-sa`; mTLS between `control-api` and LiteLLM via the existing service-mesh sidecar (already running for `rb-storage-postgres` traffic — no new mesh config).
- Replica count: 2 (HA). Resource request: 500m CPU / 512 Mi mem each; limits: 2000m / 2 Gi.
- Health: `/health/liveness` and `/health/readiness` (LiteLLM ships these); wired into Kubernetes probes; `LiteLLMUnreachable` alert added in §8.3.
- Provider credentials: stored in `rb-secrets` (existing secret manager) and mounted into the LiteLLM pod as env; **never** mounted into `control-api`. This is the chokepoint that keeps Anthropic / OpenAI / Bedrock keys out of the Rustacean process image.
- Virtual-key issuance: one LiteLLM "team" per Rustacean tenant; one virtual key per (tenant, runtime_kind) tuple; lazy-created at first session for that combination; metadata `{tenant_id, runtime_kind, rustacean_team}` so LiteLLM logs are tenant-scopeable. Budget mirrors `AGENT_TENANT_COST_PER_HOUR_USD_MICRO_CAP` (§7.3) — the runtime-side circuit-breaker is the primary; the LiteLLM-side budget is the belt to it.
- **Failure semantics:** if LiteLLM is unreachable, sessions whose `runtime_kind = opencode` immediately fail with `error_kind="llm_unavailable"`; `runtime_kind="claude_code"` sessions are unaffected (they bypass LiteLLM — see §6.4). This is the explicit isolation benefit of having Claude Code be its own provider path.

The runtime adapter trait (`crates/rb-agent-runtime`) is hosted **inside `control-api` Phase 1**, alongside the MCP and `/v1/agents/*` modules. The Phase-2 split (§3.2) lifts the adapter modules into `services/agent-runtime` together with the rest of the registry; LiteLLM stays put across the split (it is shared by either binary).

### 3.5 `pi` runtime — evaluation outcome (RUSAA-895)

§15 originally listed pi-runtime evaluation as an open question. Rev 5 closes it.

**Binary identity.** The `pi` runtime resolves to [`@mariozechner/pi-coding-agent`](https://github.com/badlogic/pi-mono) — a Node.js CLI coding agent published by Mario Zechner under the npm name `@mariozechner/pi-coding-agent` and the binary name `pi`. It is **not** pi.ai (Inflection AI's consumer chatbot) and **not** an internal Juspay/Rustacean binary; it is an open-source CLI in the same product family as Claude Code and OpenCode.

**Concrete properties** (verified against `pi --help` v0.70.6 + upstream `packages/coding-agent/README.md`):

| Property | Value |
|----------|-------|
| Install | `npm install -g @mariozechner/pi-coding-agent` (Node runtime required; not on apt/cargo/brew/Docker official) |
| Auth | (a) Subscription OAuth (Anthropic Pro/Max, OpenAI Plus/Pro, GitHub Copilot) **or** (b) provider API keys via env / `--api-key` across ~25 providers (Anthropic, OpenAI, xAI, Google, DeepSeek, Mistral, Groq, Bedrock, Vertex, …). Default provider `google`. |
| Input | CLI flags + positional args. Non-interactive: `pi -p "<prompt>"` (`--print`). System prompt via `--system-prompt` / `--append-system-prompt` (text or file). Sessions resume via `--session <path\|id>`; storage `~/.pi/<dir>/`. Stdin is **not** the primary input channel. |
| **MCP client** | **NOT SUPPORTED.** Upstream README: *"No MCP. Build CLI tools with READMEs (see Skills), or build an extension that adds MCP support."* `.mcp.json` from cwd is not honoured. Pi has its own extension/skill system instead. |
| Output | `--mode {text\|json\|rpc}`. JSON/RPC modes emit LF-delimited JSONL. Empirically observed event types: `session`, `agent_start`, `turn_start`, `message_start`, `message_update` (sub-events: `thinking_start/delta/end`, `text_start/delta/end`, `tool_use_*`, `tool_result_*`), `message_end`. RPC mode is a strict-framing variant intended for process-integration. |
| Built-in tools | `read`, `bash`, `edit`, `write`, `grep`, `find`, `ls` — executed **inside the pi process**, not delegated to a host. |

**Architectural fit assessment.** Two integration shapes are available; neither cleanly satisfies Wave 7's contract.

- **Shape A — Subprocess-CLI runtime (mirrors Paperclip's `pi_local`).** `PiRuntime` would spawn `pi --mode json -p <prompt> --provider <p> --model <m> --session-dir <dir>` from `control-api`, parse the JSONL stream into `SessionEvent`s, and resume via `--session`. **Two blocking concerns:** (1) Pi has no MCP client, so the `rb-query`-backed MCP tool surface specified in REQ-MC-01 / §6.4 is **unreachable from a pi session** — pi would call its own built-in `read`/`bash`/`edit`/`write`/`grep`/`find`/`ls` tools instead, bypassing tenant scoping entirely. (2) Pi's built-in `bash`/`edit`/`write` tools execute against the host filesystem inside pi's process; spawning pi from `control-api` therefore creates a cross-tenant filesystem-access primitive that the §7.1 threat model explicitly forbids. A sandbox layer (cgroup / namespace / chroot per session, plus disabling `bash`/`edit`/`write` via `--no-builtin-tools` while routing all tool calls through a pi extension that proxies back to `rb-query`) would resolve both — but that is multi-week work and Wave-8-scope at the earliest.

- **Shape B — LiteLLM model alias (current code).** `crates/rb-agent-runtime/src/adapters/litellm.rs::PiRuntime` is, today, a thin wrapper around `LiteLlmRuntime` that differs from `OpenCodeRuntime` only in the `kind` string and the virtual key. No `pi` binary participates in such a session — the runtime dispatches OpenAI-compatible chat completions through LiteLLM. Calling this `runtime_kind="pi"` is a **mislabel**: the audit trail records "pi" but the actual provider path is whatever LiteLLM is configured to forward (Anthropic / OpenAI / etc.). The mislabel impedes future-us when investigating cost or behaviour by `runtime_kind`.

**Decision (Rev 5).**

- **Close the `pi` runtime slot for Wave 7.** Drop `pi` from the `runtime_kind` CHECK constraint in `control.agent_sessions` (§4.1), drop `PiRuntime` from `crates/rb-agent-runtime`, drop the `pi` row from §6.4. The Wave-7 runtime set becomes **two**: `claude_code` and `opencode`.
- **Re-opening conditions** (any one is sufficient to re-open in a future ADR rev or follow-on ADR):
  1. Pi adds a built-in MCP client honoring `.mcp.json` (upstream feature request, not on the public roadmap).
  2. We design and ship a sandboxed-subprocess host for pi (cgroup + namespace + tool-call proxy extension) and accept that as a separate trust boundary distinct from the §7.1 MCP-tool boundary.
  3. We accept pi's built-in tool surface as the agent's tool surface (i.e., abandon the `rb-query`-via-MCP contract for the pi runtime), with a separate audit + cost story. This is a product-direction change, not an engineering tweak.
- **Migration impact.** Pre-implementation; no production rows exist for `runtime_kind='pi'`. Migration 010 ships with the two-value enum (`claude_code`, `opencode`); no follow-up migration needed.
- **Skeleton not drafted.** Per the re-opening conditions above, drafting a `PiAdapter` skeleton today would lock in either the bypass-MCP or the bypass-sandbox shape — both of which are explicitly rejected. The `AgentRuntime` trait remains shape-stable, so a future re-opening lands as a single-PR delta against Wave-8+ infrastructure.

This decision tightens §1's claim that the runtime adapter set "mirrors Paperclip's `claude_local`, `opencode_local`, `pi_local`" — Wave 7 mirrors the first two; the third is deferred for the trust-model reasons above. The mirror is restored when the re-opening conditions are met.

---

## 4. Database schema

Two new tables in the `control` schema (cross-tenant; `tenant_id` is a column, not a per-schema search-path). Migration 010 creates the tables; migration 011 (`011_agent_session_prompt_security.sql`) adds the `input_prompt_preview` column and the 90-day purge function (RUSAA-859).

### 4.1 `control.agent_sessions`

```sql
CREATE TABLE control.agent_sessions (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id       UUID NOT NULL REFERENCES control.tenants(id) ON DELETE CASCADE,
    user_id         UUID REFERENCES control.users(id) ON DELETE SET NULL,
    api_key_id      UUID REFERENCES control.api_keys(id) ON DELETE SET NULL,
    runtime_kind    TEXT NOT NULL CHECK (runtime_kind IN ('claude_code','opencode')),
    agent_id        TEXT NOT NULL,            -- runtime-specific identifier; for claude_code = model id (e.g. "claude-sonnet-4-6"); for opencode = LiteLLM model alias
    oauth_token_id  UUID REFERENCES control.oauth_tokens(id) ON DELETE SET NULL, -- non-null iff runtime_kind = 'claude_code'
    litellm_key_id  TEXT,                     -- LiteLLM virtual-key identifier; non-null iff runtime_kind = 'opencode'
    status          TEXT NOT NULL CHECK (status IN ('pending','running','completed','failed','canceled')),
    trace_id        TEXT NOT NULL,            -- 32-hex W3C trace ID; pinned at session start
    -- SECURITY (RUSAA-859): full prompt NEVER stored.  Only a ≤256-char preview
    -- persisted; full text forwarded to runtime in-process and not written to DB.
    input_prompt_preview  TEXT NOT NULL CHECK (char_length(input_prompt_preview) <= 256),
    input_tokens    INTEGER NOT NULL DEFAULT 0,
    output_tokens   INTEGER NOT NULL DEFAULT 0,
    cost_usd_micro  BIGINT NOT NULL DEFAULT 0, -- USD * 1e6 to avoid floats
    error_kind      TEXT,                     -- nullable; populated on failed/canceled
    error_message   TEXT,                     -- nullable; ≤ 4 KiB
    started_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at    TIMESTAMPTZ,              -- nullable until terminal
    last_event_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()  -- for idle-timeout reaper
);
CREATE INDEX agent_sessions_tenant_started_idx ON control.agent_sessions(tenant_id, started_at DESC);
CREATE INDEX agent_sessions_status_last_event_idx ON control.agent_sessions(status, last_event_at) WHERE status IN ('pending','running');
CREATE INDEX agent_sessions_trace_id_idx ON control.agent_sessions(trace_id);

-- §16 self-grill: tenant_id is set at session creation and MUST NOT change for the
-- lifetime of the session. Without this trigger a manual UPDATE could silently
-- re-tenant a session and break the audit invariant in §7.4.
CREATE OR REPLACE FUNCTION control.agent_sessions_tenant_id_immutable()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.tenant_id IS DISTINCT FROM OLD.tenant_id THEN
        RAISE EXCEPTION 'agent_sessions.tenant_id is immutable (was %, attempted %)',
            OLD.tenant_id, NEW.tenant_id
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER agent_sessions_tenant_id_immutable_trg
BEFORE UPDATE ON control.agent_sessions
FOR EACH ROW EXECUTE FUNCTION control.agent_sessions_tenant_id_immutable();
```

`tenant_id` exists for join + listing; **the auth chokepoint is `AuthContext`**, not this column. The trigger above is the database-level belt to the application-level braces — closes the only path by which the column could drift after creation.

**Prompt security (RUSAA-859 / H-1):** `input_prompt_preview` stores at most 256 Unicode code points of the opening message. The application layer (`sessions.rs: prompt_preview()`) truncates before the INSERT; the CHECK constraint is the DB-level backstop. The full prompt is held in process memory for the lifetime of the session task and is never written to any persistent store. Retention: `agents.purge_old_agent_sessions()` (migration 011) deletes terminal-state sessions older than 90 days, consistent with the `agent_events` partition-drop window (§4.2). Production cron schedule: `0 2 * * *` UTC.

### 4.2 `control.agent_events`

Append-only; mirrors `audit.audit_events` shape. Partitioned by month (RANGE partition on `ts`) so retention purge is `DROP PARTITION`.

```sql
CREATE TABLE control.agent_events (
    id          BIGSERIAL,
    session_id  UUID NOT NULL,
    tenant_id   UUID NOT NULL,                -- denormalized for partition pruning + tenant scan
    seq         INTEGER NOT NULL,             -- monotonic per session, gap-free
    type        TEXT NOT NULL,                -- see §5 envelope schema
    data        JSONB NOT NULL,               -- typed payload per event kind
    ts          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (id, ts)
) PARTITION BY RANGE (ts);
CREATE INDEX agent_events_session_seq_idx ON control.agent_events(session_id, seq);
CREATE INDEX agent_events_tenant_ts_idx ON control.agent_events(tenant_id, ts DESC);

-- INSERT-only writer role; mirrors `rb_audit_writer` from migrations/control/006_audit_events.sql.
-- The control-api process connects with this role for agent_events writes and uses the
-- migration-owner role for normal control-schema reads (agent_sessions list/get, etc.).
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'rb_agent_writer') THEN
        CREATE ROLE rb_agent_writer NOLOGIN;
    END IF;
END
$$;

GRANT USAGE ON SCHEMA control TO rb_agent_writer;
GRANT INSERT, SELECT ON control.agent_events TO rb_agent_writer;
-- Belt: explicit denial on the table for everyone but the owner.
REVOKE UPDATE, DELETE ON control.agent_events FROM PUBLIC;

-- First two monthly partitions created in the migration; future ones via the
-- partition-rollover cron described in §4.3.
```

`(session_id, seq)` is the replay key. SSE clients reconnect with `Last-Event-ID = "{session_id}:{seq}"`; the resume handler selects events `WHERE session_id = $1 AND seq > $2`.

Retention: 90 days default, controlled by partition drop schedule (matches audit). Same retention, same justification.

### 4.3 Migration safety

- All three tables are NEW — no backfill, no concurrent-write hazard.
- Foreign keys to `control.tenants` and `control.users` are `ON DELETE CASCADE` / `ON DELETE SET NULL`; tenant deletion (REQ-TN-04, ADR-008) sweeps agent + OAuth state automatically. Idempotent.
- `agent_events` partitioning uses `pg_partman` if available, otherwise the migration creates `agent_events_2026_05`, `agent_events_2026_06` directly + a `pg_cron` job to roll new partitions monthly. Fallback path documented in the migration.
- `oauth_tokens` rows are encrypted at the column level (see §4.4 / §7.5); the migration creates the row-level encryption helpers under the same `pgcrypto` extension already loaded by Wave 4.

### 4.4 `control.oauth_tokens`

NEW table to hold per-user Claude OAuth refresh tokens (so a `claude_code` session can mint short-lived access tokens against the user's own Claude Max plan). One row per (user_id, provider). Encrypted at rest.

```sql
CREATE TABLE control.oauth_tokens (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id       UUID NOT NULL REFERENCES control.tenants(id) ON DELETE CASCADE,
    user_id         UUID NOT NULL REFERENCES control.users(id) ON DELETE CASCADE,
    provider        TEXT NOT NULL CHECK (provider IN ('claude')),  -- enum widens later (openai, github, ...)
    -- AEAD-encrypted ciphertext; key from rb-secrets (key_id pinned per row for rotation).
    refresh_token_ciphertext  BYTEA  NOT NULL,
    access_token_ciphertext   BYTEA,                  -- nullable; cached short-lived token
    access_token_expires_at   TIMESTAMPTZ,            -- nullable until first mint
    encryption_key_id         TEXT NOT NULL,          -- references rb-secrets KMS key id
    scopes                    TEXT[] NOT NULL DEFAULT '{}',
    last_refreshed_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at                TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    revoked_at                TIMESTAMPTZ,            -- soft-delete; revoked tokens fail closed
    UNIQUE (user_id, provider)
);
CREATE INDEX oauth_tokens_tenant_user_idx ON control.oauth_tokens(tenant_id, user_id);
CREATE INDEX oauth_tokens_provider_expiry_idx
    ON control.oauth_tokens(provider, access_token_expires_at)
    WHERE access_token_expires_at IS NOT NULL AND revoked_at IS NULL;

-- Read role for the runtime to mint access tokens; INSERT/UPDATE for the OAuth callback handler.
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'rb_oauth_writer') THEN
        CREATE ROLE rb_oauth_writer NOLOGIN;
    END IF;
END
$$;
GRANT USAGE ON SCHEMA control TO rb_oauth_writer;
GRANT SELECT, INSERT, UPDATE ON control.oauth_tokens TO rb_oauth_writer;
REVOKE DELETE ON control.oauth_tokens FROM PUBLIC;  -- revoke is soft-delete via revoked_at
```

Encryption design and rotation flow are detailed in §7.5. Tokens are decrypted **inside the runtime adapter only** for the duration of a session; raw plaintext never touches `agent_events`, `audit_events`, or any log surface.

---

## 5. Event envelope (REQ-MC-02)

All events flow through `rb-sse` `EventBus::publish`. The SSE `event:` field is `agent.event` for **every** kind; the actual type is in the JSON body's `type` key. (One SSE event name keeps the browser `EventSource` listener registration trivial; payload polymorphism stays in JSON.)

### 5.1 Schema

```jsonc
// All events:
{
  "type":       "<one of session_started | tool_call_started | tool_call_completed | tool_call_failed | llm_call_started | llm_call_completed | message | session_completed | session_failed | session_canceled>",
  "session_id": "<uuid>",
  "seq":        <int, monotonic per session>,
  "ts":         "<RFC3339 utc>",
  "data":       { /* per-type payload — schema below */ }
}
```

| `type` | `data` shape |
|--------|--------------|
| `session_started` | `{ agent_id, input_prompt_preview, trace_id }` (preview = first 256 chars) |
| `tool_call_started` | `{ tool_name, args }` (args validated against tool schema; trimmed to ≤ 4 KiB) |
| `tool_call_completed` | `{ tool_name, duration_ms, result_summary }` (result_summary = ≤ 1 KiB stringified preview) |
| `tool_call_failed` | `{ tool_name, duration_ms, error_kind, error_message }` |
| `llm_call_started` | `{ model, input_tokens }` |
| `llm_call_completed` | `{ model, input_tokens, output_tokens, latency_ms, cost_usd_micro }` |
| `message` | `{ role: "assistant" \| "user", text }` (text trimmed to ≤ 16 KiB; full text persisted to `agent_events.data.text_full` only when ≤ 16 KiB; longer messages reference `rb-blob://...`) |
| `session_completed` | `{ summary, final_message, total_input_tokens, total_output_tokens, total_cost_usd_micro }` |
| `session_failed` | `{ error_kind, error_message, total_input_tokens, total_output_tokens, total_cost_usd_micro }` |
| `session_canceled` | `{ canceled_by_user_id, total_input_tokens, total_output_tokens, total_cost_usd_micro }` |

`error_kind` enum: `tool_validation`, `tool_runtime`, `llm_timeout`, `llm_rate_limit`, `llm_other`, `budget_exceeded`, `wall_clock_exceeded`, `idle_timeout`, `internal`.

### 5.2 Persistence ordering

For each event, the runtime writes to `control.agent_events` **first**, then publishes to `EventBus`. If the DB write succeeds and the bus publish fails (rare), the next reconnect via `Last-Event-ID` replays from the DB so the client sees no gap. If the DB write fails, the event is logged + dropped + the session is marked `failed` (event log integrity is more important than continuing the session). Documented behaviour, not silent.

---

## 6. REST + MCP contracts

All endpoints under `/v1`, all `Content-Type: application/json` (except SSE), all carry `request-id` and OTel trace context. Errors envelope:

```json
{ "error": { "code": "<machine_code>", "message": "<human>" } }
```

Wave-7 error codes: `unauthorized`, `email_not_verified`, `session_expired`, `insufficient_role`, `insufficient_scope`, `not_found`, `bad_request`, `tenant_isolation_violation`, `agent_session_not_running`, `agent_session_not_found`, `agent_budget_exceeded`, `agent_idle_timeout`, `agent_wall_clock_exceeded`, `mcp_invalid_request`, `mcp_method_not_found`, `mcp_invalid_params`, `mcp_internal_error`, `mcp_tool_validation`, `llm_unavailable`, `llm_timeout`, `llm_rate_limit`, `rate_limit_exceeded`.

### 6.1 REQ-MC-02 — Session lifecycle (REST)

```
POST /v1/agents/sessions
Auth: session (verified) OR API-key (agent | admin)
Body: {
  "runtime_kind":  "<one of 'claude_code' | 'opencode'>",
  "agent_id":      "<string; runtime-specific — see §6.4>",
  "input_prompt":  "<string, 1..65536 chars>",
  "input_tokens_budget":   <int, default 100000, max 500000>,
  "output_tokens_budget":  <int, default 20000,  max 100000>,
  "wall_clock_seconds":    <int, default 3600,   max 3600>
}
Response 201: {
  "id":           "<uuid>",
  "tenant_id":    "<uuid>",
  "runtime_kind": "<echoed>",
  "trace_id":     "<32-hex>",
  "status":       "pending",
  "started_at":   "<rfc3339>",
  "events_url":   "/v1/agents/sessions/<uuid>/events"
}
Errors: 400 bad_request (budget out of range, unknown runtime_kind, agent_id not allowed for runtime),
        401 oauth_required (runtime_kind='claude_code' but no live OAuth token for caller),
        403 insufficient_scope (API key without `agent`), 403 runtime_disabled (runtime_kind not in AGENT_RUNTIMES_ENABLED),
        429 rate_limit_exceeded, 503 llm_unavailable (LiteLLM unreachable AND runtime_kind = 'opencode').
```

```
GET  /v1/agents/sessions          // list, paginated, filter by status / agent_id / since
GET  /v1/agents/sessions/{id}     // session metadata + token totals
DELETE /v1/agents/sessions/{id}   // cancel; idempotent; 200 with current status
```

```
GET  /v1/agents/sessions/{id}/events
Auth: session OR API-key (agent | admin), AND must match session's tenant_id
Headers (response): Content-Type: text/event-stream; X-Accel-Buffering: no
Headers (request, optional): Last-Event-ID: <session_id>:<seq>
Stream: SSE; event: agent.event; data: <JSON envelope per §5.1>
        — replays events from DB after Last-Event-ID, then live tails via EventBus
        — keep-alive heartbeat every 15s as `: ping`
        — terminates on session terminal event OR client disconnect
```

### 6.2 REQ-MC-01 — MCP server

```
POST /mcp
Auth: API-key (agent | admin); session cookie also accepted to support browser-side MCP client (deferred client work — §15)
Content-Type: application/json
Headers: Mcp-Session-Id: <uuid?> (echoed; binds MCP session to agent_sessions row when present)
Body: JSON-RPC 2.0 request
```

Supported JSON-RPC methods (initial set):

| Method | Behaviour |
|--------|-----------|
| `initialize` | Returns server info + protocol version; binds the `Mcp-Session-Id` to the caller's `AuthContext` (tenant + user). |
| `tools/list` | Returns the 6 tool definitions with JSON schemas. |
| `tools/call` | Dispatches to the named tool; arg validation against schema; result wrapped in MCP `CallToolResult`. |
| `ping` | Liveness; echoes input. |
| `notifications/initialized` | Lifecycle ack. |
| `logging/setLevel` | Per-session log filter; affects only the audit-event verbosity for this session. |

Non-supported (Wave 7): `prompts/*`, `resources/*`, `sampling/*` — deferred to Wave 8+ once we have a tools-only surface stable.

#### 6.2.1 Tool definitions (REQ-MC-01)

Each tool is a thin axum-internal call into `rb-query`; argument schemas are stable JSON Schema published via `tools/list`.

| Tool | rb-query call | Args (JSON Schema summary) | Result |
|------|----------------|----------------------------|--------|
| `search_items` | `rb_query::semantic::search` | `{ query: string, repo_id?: uuid, kind?: ItemKind[], limit?: int (1..100, default 20), score_floor?: float (0..1, default 0.20) }` | `{ results: SearchHit[], next_cursor?: string }` (same shape as REQ-DP-01) |
| `get_item` | `rb_query::pg::items::get` | `{ repo_id: uuid, fqn: string }` | `ItemDetail` (same shape as REQ-DP-02) |
| `get_callers` | `rb_query::graph::traversal::callers` | `{ repo_id: uuid, fqn: string, depth?: int (1..10, default 3), limit?: int (1..1000, default 200) }` | Same shape as REQ-DP-03 |
| `get_callees` | `rb_query::graph::traversal::callees` | (same shape as `get_callers`) | Same shape as REQ-DP-03 |
| `get_trait_impls` | `rb_query::graph::impls::for_trait` | `{ repo_id: uuid, trait_fqn: string }` | Same shape as REQ-DP-04 |
| `run_query` | `rb_query::graph::raw::cypher` | `{ cypher: string, params?: object, read_only?: bool (default true) }` | Same shape as REQ-DP-05 |

`run_query` requires API-key scope `admin` (matches REQ-DP-05); even when invoked through MCP the scope check is enforced server-side. If a session was started with only `agent` scope, `run_query` calls return `mcp_method_not_found` style error (`code = -32601`) with a structured `data.reason = "insufficient_scope"` so the agent can fall back gracefully without leaking the tool's existence.

#### 6.2.2 Tenant binding for MCP

- `initialize` binds `Mcp-Session-Id` to the caller's `AuthContext.tenant_id`.
- Every `tools/call` re-extracts the caller's `AuthContext` and **rejects** the call (`mcp_invalid_request`, `data.reason="tenant_drift"`) if the tenant_id has changed since `initialize`. This catches the case where a long-lived MCP session crosses a user's tenant switch.
- Every `tools/call` is written to `audit.audit_events` with `(action="mcp.tool.call", tool_name, args_hash, result_status)`. Args are SHA-256-hashed at audit time so the audit log never persists the raw query (PII / secret-leak hygiene); the full args live in `agent_events.data` only for the lifetime of `agent_events` retention (90 days).

### 6.3 OAuth surface for `runtime_kind="claude_code"`

```
GET  /v1/auth/oauth/claude/start?redirect_uri=<url>
     → 302 to Anthropic's OAuth authorize endpoint (PKCE; state cookie set; user-bound)

GET  /v1/auth/oauth/claude/callback?code=...&state=...
     → exchanges code for refresh+access; encrypts and INSERT/UPDATEs control.oauth_tokens
     → 302 back to redirect_uri with `?status=success` (no tokens in URL)

DELETE /v1/auth/oauth/claude
     → soft-revokes the caller's row (sets revoked_at); subsequent claude_code sessions get oauth_required.
```

**Implementation notes — `redirect_uri` validation (open-redirect guard):**

The `redirect_uri` query parameter to `GET /v1/auth/oauth/claude/start` is **optional**.
When supplied, the server validates that its origin (scheme + host + port) exactly matches
the origin of `RB_BASE_URL`.  Requests whose `redirect_uri` has a different origin are
rejected immediately with `400 bad_redirect_uri`; the OAuth flow is never initiated.

- Non-HTTP/S schemes (e.g. `javascript:`, `data:`) are unconditionally rejected.
- The comparison is: `scheme://host[:port]` — path, query, and fragment are ignored for the origin check.
- When `redirect_uri` is absent the callback falls back to `{RB_BASE_URL}/settings/integrations?oauth=claude&status=success`.
- The validated `redirect_uri` is stored base64url-encoded inside the `rb_pkce_state` cookie alongside the PKCE `code_verifier` and `state`; the callback decodes and uses it for the final redirect.

### 6.4 Per-runtime dispatch (REQ-MC-02 internal contract)

The `AgentRegistry` resolves `runtime_kind → AgentRuntime impl` from `crates/rb-agent-runtime` at session-creation time. Every session has exactly one runtime adapter; **runtime cannot change mid-session** (immutability mirrored from `tenant_id` per §6.2.2).

| `runtime_kind` | Adapter | Auth source | Provider path | `agent_id` semantics | Cost owner |
|----------------|---------|-------------|---------------|----------------------|------------|
| `claude_code` | `ClaudeCodeRuntime` | `oauth_tokens` row for `(user_id, provider='claude')`; access token minted on demand | Direct → Anthropic Messages API | Anthropic model id (e.g. `claude-sonnet-4-6`); allow-list checked at session create | **User's Claude Max plan** (no Rustacean billing) |
| `opencode` | `OpenCodeRuntime` | LiteLLM virtual key (one per `(tenant_id, runtime_kind)`) | Through LiteLLM → underlying provider | LiteLLM model alias (e.g. `opencode/sonnet`); allow-list = LiteLLM `model_list` for the tenant's team | Tenant (LiteLLM tracks; Rustacean cost circuit-breaker §7.3 mirrors) |

> **Pi deferred (rev 5).** A `pi` row was previously listed here. It has been removed; see §3.5 for the binary-identity, MCP-absence, and host-tool sandbox findings that justify deferring the slot for Wave 7.

**Resolution sequence at `POST /v1/agents/sessions`:**

1. Validate `runtime_kind` is in `AGENT_RUNTIMES_ENABLED` (env config; allows ops to disable a runtime cluster-wide without a code change).
2. If `runtime_kind="claude_code"`: load `oauth_tokens` row for the authenticated user. If missing or `revoked_at IS NOT NULL`, return 401 `oauth_required` with a body pointing the caller to `/v1/auth/oauth/claude/start`. Stamp `oauth_token_id` on the session row.
3. If `runtime_kind="opencode"`: lookup-or-create the tenant's LiteLLM virtual key for this runtime (LiteLLM admin API, idempotent by metadata). Stamp `litellm_key_id` on the session row.
4. Validate `agent_id` against the runtime's allow-list (`claude_code` → fixed Anthropic model list; `opencode` → LiteLLM `model_list` query result, cached 60s).
5. Insert `agent_sessions` row, open root span, return 201 with `runtime_kind` echoed.

**Tool-call wiring (per-runtime):** every tool call inside a session is dispatched the same way regardless of runtime. The runtime adapter, when the LLM emits a tool-use, calls a host-supplied callback `dispatch_tool(session_id, tool_name, args) -> ToolResult`. The host implementation lives in `services/control-api/src/agents/tool_dispatch.rs` and is the single place where tools resolve to `rb-query` calls. Adapters know nothing about `rb-query`.

**Failure-mode matrix:**

| Failure | `claude_code` | `opencode` |
|---------|---------------|------------|
| Anthropic API down | session fails `llm_unavailable` | unaffected |
| LiteLLM down | unaffected | session fails `llm_unavailable` |
| User OAuth token expired (refresh succeeds) | adapter refreshes silently | n/a |
| User OAuth token revoked / refresh fails | session fails `oauth_required`; existing sessions in flight emit `session_failed{error_kind:"oauth_revoked"}` | n/a |
| Tenant LiteLLM virtual key suspended (budget exhausted at LiteLLM layer) | unaffected | session fails `budget_exceeded` (LiteLLM-reported) |

This matrix is the operational story the runbook in §13 must render.

### 6.5 OpenAPI + frontend types

Every endpoint (REST + the `/mcp` envelope) is annotated `#[utoipa::path(...)]`; `scripts/check-openapi-sync.sh` enforces `frontend/src/api/generated/schema.ts` is regenerated. The MCP JSON-RPC types are encoded as one `MCPRequest` / `MCPResponse` schema discriminated by `method`; the frontend uses these for type-safe MCP calls.

---

## 7. Security model

### 7.1 Threat model (named, then mitigated)

| Threat | Mitigation |
|--------|------------|
| Prompt-injection convinces the LLM to call a tool with a different `tenant_id` | tenant_id is **never** read from tool args; always taken from server-side `AuthContext`. (§6.2.2) |
| User switches tenants mid-session and the agent keeps querying old-tenant data | `tools/call` re-extracts `AuthContext` and rejects on drift. (§6.2.2) |
| Runaway agent loops the LLM until budget exhausted by the user — tenant gets billed forever | per-session token budget + per-session wall-clock cap + per-tenant rate limits + circuit breaker on cost-per-tenant-per-hour. (§7.3) |
| Compromised API key leaks read access to *all* code | `agent` scope is narrower than `admin`; `run_query` still requires `admin`. Auditable + revocable. (§7.2) |
| Compromised API key floods sessions to drain LLM provider quota | per-key rate limit + per-tenant rate limit + global concurrent-session semaphore + cost circuit-breaker. (§7.3) |
| Adversarial Cypher via `run_query` leaks cross-tenant data | already mitigated by ADR-008 §3.5: `run_query` routes through `TenantGraph::run` with AST injection + EXPLAIN-plan write check. Wave 7 changes nothing here; adds audit log entry. |
| Tool-call result contains PII / secret that gets persisted in `agent_events` | `agent_events` retention is 90 days; results ≤16 KiB inlined, larger blob-refed. The audit log only stores `args_hash`, not the result. The `agent_events` table is single-tenant-scoped at every read. Documented; not a Wave-7 mitigation. |
| Browser session token exfiltrated → attacker drives sessions as the user | unchanged from data-plane: existing CSRF posture, `Secure;HttpOnly` cookie, session expiry. Wave 7 adds nothing; same posture. |
| Malicious agent instructs user to share an artifact across tenants | out-of-band; agent has no inter-tenant tool. The session's tool surface IS the trust boundary. (§7.4) |

### 7.2 API-key scope additions

```rust
pub enum Scope { Read, Write, Admin, Agent }   // NEW: Agent
```

- `Agent` is **disjoint** from `Read`/`Write`/`Admin` — a key can carry `[Agent]` only, `[Read, Agent]`, etc.
- Migration: `migrations/control/010_agent_sessions.sql` adds `'agent'` to the existing scope text-set check constraint.
- Backwards-compat: existing keys remain unaffected; explicit upgrade required to use agent endpoints.
- Audit: scope additions / removals already logged via existing API-key admin routes.

### 7.3 Rate limiting + budgets

- **Per-tenant concurrent sessions**: hard cap 100, configurable via `AGENT_MAX_SESSIONS_PER_TENANT`. Implemented via per-tenant `tokio::sync::Semaphore` keyed off `tenant_id`. New session beyond cap → 429 `rate_limit_exceeded`.
- **Per-tenant session-create rate**: token bucket, default 10 / min, 30-burst. Existing `tower_governor` middleware (already in place for `/v1/auth/login`) extended to `/v1/agents/sessions` POST.
- **Per-session token budgets**: hard-fail at the configured limit; emits `session_failed{error_kind:"budget_exceeded"}`.
- **Per-session wall-clock**: hard-fail at the configured limit; emits `session_failed{error_kind:"wall_clock_exceeded"}`.
- **Per-tenant cost circuit-breaker**: if `SUM(cost_usd_micro) WHERE tenant_id=$1 AND started_at >= NOW() - '1 hour'` > `AGENT_TENANT_COST_PER_HOUR_USD_MICRO_CAP` (default 100 USD ⇒ 1e8 micro-USD), new session creates return 429 `rate_limit_exceeded` with `Retry-After`. Computed on each session start; result cached for 60s per tenant.

### 7.4 Audit invariant

For every session, the following must hold (test-asserted):

```sql
SELECT count(*) FROM audit.audit_events
WHERE actor_tenant_id = $session_tenant
  AND action LIKE 'agent.%'
  AND occurred_at BETWEEN $session.started_at AND COALESCE($session.completed_at, NOW())
```

equals the count of session-level + tool-call events for that session. A drift means an agent action escaped audit and is treated as a P1 incident.

### 7.5 OAuth (Claude) + LiteLLM virtual-key scoping

**Claude OAuth — token lifecycle and storage.**

- Flow: standard OAuth 2.0 Authorization Code with PKCE. Client id is Rustacean's registered Anthropic OAuth app; user authenticates against their own Claude account (Max plan or otherwise) and consents to the Rustacean scopes (`messages:write`, `usage:read`).
- State cookie is `__Host-`-prefixed, `Secure;HttpOnly;SameSite=Lax`, 10-minute TTL, signed with the existing `rb-secrets` HMAC key. Defeats CSRF on `/callback`.
- Tokens at rest: `refresh_token_ciphertext` and `access_token_ciphertext` are AES-256-GCM with a per-row 96-bit nonce; the AEAD key is derived from `rb-secrets` KMS key id `oauth-claude-v1` via HKDF with `user_id` as info. The `encryption_key_id` column pins the KMS key version; rotation = decrypt-old + re-encrypt-new in a background job (out-of-scope for Wave 7 — we ship `v1` and revisit when KMS rotation policy lands).
- Tokens in transit: only between control-api and Anthropic OAuth endpoints over TLS; never logged, never sent to the browser, never stored in `agent_events` or `audit.audit_events`.
- Refresh: the runtime adapter holds a decrypted access token in memory for the session's lifetime; if the access token has < 60s of validity at any tool/LLM call, the adapter triggers a refresh, persists the new ciphertext, and continues. Refresh errors fail the session with `error_kind="oauth_revoked"` and surface a `session_failed` event.
- Revocation: `DELETE /v1/auth/oauth/claude` sets `revoked_at`. Active sessions for that user complete naturally (best-effort) or fail at next refresh; new sessions for the user with `runtime_kind="claude_code"` immediately return `oauth_required`.
- Audit: every OAuth start / callback / revoke / refresh is written to `audit.audit_events` with `(action="oauth.claude.{start|callback|revoke|refresh}", actor_user_id, actor_tenant_id, outcome)`. Refresh events are written at **100%** — no sampling (RUSAA-861, security finding M-1). Sampling security-relevant events prevents detection of anomalous refresh patterns (e.g. a compromised session refreshing from a second IP). If write volume becomes a concern, aggregate into a `oauth_refresh_daily_counts` summary table rather than dropping individual audit records.

**LiteLLM virtual keys — issuance and scoping.**

- One LiteLLM "team" per Rustacean tenant; team metadata `{rustacean_tenant_id}`. Created lazily at first non-Claude session creation for the tenant and cached.
- One virtual key per `(tenant_id, runtime_kind)` tuple within the team. Key metadata `{rustacean_tenant_id, runtime_kind, owner_agent_id}`.
- Key spend cap: mirrored from `AGENT_TENANT_COST_PER_HOUR_USD_MICRO_CAP` divided into LiteLLM's per-key budget API (LiteLLM enforces hourly + daily ceilings). The Rustacean-side circuit-breaker (§7.3) is the primary enforcement; LiteLLM's cap is a backstop that catches a stuck control-api.
- Key rotation: tenant-admin REST endpoint `POST /v1/auth/litellm/rotate?runtime_kind=...` (admin scope) re-issues the key and supersedes the cached entry. Rotation invalidates active runtime adapters' cached keys at next call (cache TTL 60s).
- Provider credentials (Anthropic, OpenAI, etc.) live **inside LiteLLM** only, sourced from `rb-secrets` via the LiteLLM env config. Rustacean control-api process never sees a raw provider key.
- Audit: virtual-key issuance / rotation / revocation logged via LiteLLM's own audit log shipped to Loki (existing log pipeline) **and** mirrored into `audit.audit_events` by the issuance code path so we can run §7.4-style invariant checks.
- Failure semantics already covered in §3.4 / §6.4 (LiteLLM unreachable ⇒ `llm_unavailable` only for non-Claude runtimes).

**Why two key surfaces (OAuth + LiteLLM) instead of one.** Claude Code's value proposition is "use the user's own Max-plan quota". That mandates user-bound OAuth — no virtual key can stand in. LiteLLM, conversely, gives us provider-uniform abstraction *without* user binding for OpenCode and Pi. Trying to fit both under one mechanism would either route Claude Code through LiteLLM (defeating the no-API-key-spend property) or push OpenCode/Pi onto OAuth (exploding per-user provisioning). The two surfaces are the smallest correct decomposition.

---

## 8. Deployment, config, observability

### 8.1 New env vars (control-api)

Runtime topology + LiteLLM + OAuth (rev 3):

| Var | Default | Purpose |
|-----|---------|---------|
| `AGENT_RUNTIMES_ENABLED` | `claude_code,opencode` | comma-separated allow-list; ops can disable a runtime cluster-wide (e.g. emergency Claude OAuth pause) |
| `AGENT_DEFAULT_RUNTIME` | `claude_code` | runtime used when request omits `runtime_kind`; must be in `AGENT_RUNTIMES_ENABLED` |
| `AGENT_DEFAULT_MODEL_BY_RUNTIME` | `claude_code=claude-sonnet-4-6,opencode=opencode/sonnet` | per-runtime fallback when request omits `agent_id` |
| `LITELLM_BASE_URL` | `http://litellm.litellm.svc:4000` | in-cluster LiteLLM ClusterIP DNS |
| `LITELLM_ADMIN_API_KEY` | — | secret loaded via `rb-secrets`; used to issue/rotate tenant virtual keys; never on hot tool-call path |
| `LITELLM_TIMEOUT_SECONDS` | `30` | per-LLM-call HTTP timeout (LiteLLM may chain its own retries inside) |
| `LITELLM_VIRTUAL_KEY_CACHE_TTL_SECONDS` | `60` | cache lifetime for the per-tenant key id stamped on `agent_sessions.litellm_key_id` |
| `LITELLM_TEAM_PREFIX` | `rustacean` | LiteLLM team naming prefix; team name = `{prefix}-{tenant_id}` |
| `OAUTH_CLAUDE_CLIENT_ID` | — | secret; Anthropic OAuth app client id |
| `OAUTH_CLAUDE_CLIENT_SECRET` | — | secret; Anthropic OAuth app client secret |
| `OAUTH_CLAUDE_REDIRECT_URI` | `https://<frontend-host>/v1/auth/oauth/claude/callback` | must match what's registered with Anthropic; reads `RB_BASE_URL` |
| `OAUTH_CLAUDE_KMS_KEY_ID` | `oauth-claude-v1` | `rb-secrets` KMS key id for AEAD of stored tokens |
| `OAUTH_CLAUDE_TOKEN_REFRESH_LEAD_SECONDS` | `60` | refresh access token if remaining lifetime < this |

Capacity / budget / lifecycle (unchanged from rev 2):

| Var | Default | Purpose |
|-----|---------|---------|
| `AGENT_MAX_SESSIONS_PER_PROCESS` | `200` | semaphore cap |
| `AGENT_MAX_SESSIONS_PER_TENANT` | `100` | per-tenant cap |
| `AGENT_TENANT_COST_PER_HOUR_USD_MICRO_CAP` | `100000000` (= $100/h) | circuit-breaker cap; mirrored into LiteLLM virtual-key budget |
| `AGENT_IDLE_TIMEOUT_SECONDS` | `900` | reaper threshold |
| `AGENT_WALL_CLOCK_MAX_SECONDS` | `3600` | hard cap |

Removed in rev 3 (replaced by runtime + LiteLLM topology): `AGENT_LLM_PROVIDER`, `AGENT_LLM_BASE_URL`, `AGENT_LLM_API_KEY`, `AGENT_DEFAULT_MODEL`. The single-provider env shape was a working assumption; the board directive supersedes it.

### 8.2 Metrics (Prometheus)

All new metrics carry `tenant_id` + `agent_id` labels (high-cardinality risk acknowledged; mitigated by Wave-6 prometheus federation pattern).

All runtime-aware metrics carry an additional `runtime_kind` label (`claude_code` | `opencode`) so we can split spend, latency, and failure by runtime.

- `agent_sessions_total{status, runtime_kind}` (counter)
- `agent_sessions_active{runtime_kind}` (gauge)
- `agent_session_duration_seconds{outcome, runtime_kind}` (histogram, buckets 1, 5, 30, 60, 300, 1800, 3600)
- `agent_tokens_total{direction=input|output, runtime_kind}` (counter)
- `agent_cost_usd_micro_total{runtime_kind}` (counter; `claude_code` always emits 0 — user-paid)
- `agent_tool_calls_total{tool, outcome, runtime_kind}` (counter)
- `agent_tool_call_duration_seconds{tool, runtime_kind}` (histogram)
- `agent_llm_calls_total{model, outcome, runtime_kind}` (counter)
- `agent_llm_call_duration_seconds{model, runtime_kind}` (histogram)
- `agent_budget_exhausted_total{kind=input|output|wall_clock|idle|tenant_cost, runtime_kind}` (counter)
- `mcp_requests_total{method, outcome}` (counter)
- `mcp_request_duration_seconds{method}` (histogram)
- `agent_litellm_requests_total{outcome}` (counter; tracked from the control-api side, complements LiteLLM's own metrics)
- `agent_litellm_request_duration_seconds` (histogram)
- `agent_oauth_claude_refresh_total{outcome=success|failure_revoked|failure_other}` (counter)
- `agent_oauth_claude_active_tokens` (gauge; non-revoked rows in `oauth_tokens` for `provider='claude'`)

### 8.3 Alerts (initial)

- `AgentTenantCostBreach` — `rate(agent_cost_usd_micro_total[1h]) > AGENT_TENANT_COST_PER_HOUR_USD_MICRO_CAP * 0.8` for 10m → page Platform Engineer.
- `AgentSessionFailureRate` — `rate(agent_sessions_total{status="failed"}[15m]) / rate(agent_sessions_total[15m]) > 0.20` → page Services Engineer.
- `MCPProtocolErrorRate` — `rate(mcp_requests_total{outcome="error"}[15m]) / rate(mcp_requests_total[15m]) > 0.05` → page.
- `AgentIdleReaperLag` — `agent_sessions_active{status="running"}` rising while `agent_session_duration_seconds_count` flat for 30m → indicates reaper stuck.
- `AgentEventsPartitionLag` — fires when no partition exists for `(NOW() + interval '7 days')::date`. Implementation: a small probe query `SELECT 1 FROM pg_inherits WHERE inhparent = 'control.agent_events'::regclass AND inhrelid::regclass::text LIKE 'control.agent_events_' || to_char(NOW() + interval '7 days', 'YYYY_MM')` exposed as Prometheus gauge `agent_events_next_partition_ready` (1 = ready, 0 = missing); alert on `agent_events_next_partition_ready == 0` for 1h → page Platform Engineer. Catches the case where the partition-rollover cron fails silently — without this, inserts would start failing 7 days later.
- `LiteLLMUnreachable` — `up{job="litellm"} == 0` for 2m, **OR** `rate(agent_litellm_requests_total{outcome="error"}[5m]) / rate(agent_litellm_requests_total[5m]) > 0.20` for 5m → page Platform Engineer. Distinguishes "service down" from "service degraded"; either is page-worthy because every `opencode` session creation fails closed.
- `OAuthClaudeRefreshFailureRate` — `rate(agent_oauth_claude_refresh_total{outcome="failure_revoked"}[15m]) / rate(agent_oauth_claude_refresh_total[15m]) > 0.10` for 10m → page Services Engineer. A spike here typically means Anthropic invalidated tokens en masse (provider incident) or our refresh call signature broke after an SDK upgrade.

### 8.4 Tracing

Reuses ADR-008 / ADR-007 OTel pipeline. New attribute conventions follow OpenTelemetry GenAI semantic conventions where possible:

- Root span: `agent.session.run`, attrs `tenant.id`, `agent.id`, `session.id`, `gen_ai.system`, `gen_ai.request.model`.
- Tool span: `agent.tool.<tool_name>`, attrs `tool.name`, `tool.args.size_bytes`, `tool.result.size_bytes`, `tool.outcome`.
- LLM span: `gen_ai.client.operation` (per OTel spec), attrs `gen_ai.system`, `gen_ai.request.model`, `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`, `gen_ai.response.finish_reasons`, `gen_ai.cost.usd_micro` (custom, not in OTel spec yet).

Trace propagation: an MCP `tools/call` carries the agent root span as parent; the tool's underlying `rb-query` calls inherit the trace context, so a single trace covers session → tool call → DB query → response.

---

## 9. Tracer-bullet plan

The Wave 7 vertical slice (REQ-MC-01-min + REQ-MC-02-min + REQ-FE-06-first-slice) is **one issue** spanning all three requirements:

> **RUSAA-NNN (placeholder)** — *"User starts an agent session via the UI and watches `search_items` + `get_item` tool calls stream live."*
>
> - DB migration: `migrations/control/010_agent_sessions.sql` (three tables — `agent_sessions`, `agent_events`, `oauth_tokens` — scope addition + first two `agent_events` partitions).
> - `crates/rb-mcp` — minimal protocol library with `initialize`, `tools/list`, `tools/call` for `search_items` + `get_item` only.
> - `crates/rb-agent-runtime` — `AgentRuntime` trait + the **single** `ClaudeCodeRuntime` adapter for the tracer. `OpenCodeRuntime` ships in the wave but after the tracer (one follow-on issue); the trait is mandatory at tracer time so wiring isn't a follow-up refactor. (Pi was evaluated and deferred — see §3.5.)
> - `services/control-api` — `routes/mcp/` (one POST handler), `routes/agents/` (POST sessions, GET /events SSE), `routes/auth/oauth/claude/` (start + callback only; revoke later), `state::AgentRegistry` minimal.
> - LiteLLM: chart shipped, deployed, smoke-tested with a synthetic OpenCode session, but **not exercised** in the tracer assertion (the tracer is `runtime_kind="claude_code"` only). The two LiteLLM-backed runtimes' E2E land in follow-on issues within the same wave.
> - Frontend — `/agents` route with "start a session" button (Claude Code only at tracer time; runtime selector hidden), `/agents/$id` route with live event log (no replay/cancel UI yet).
> - Tracer asserts: from a fresh empty repo's projection (Wave-5 fixture), a user with a connected Claude OAuth token can ask "find Vec::push" via the `claude_code` runtime and see exactly the search→get-item event pair stream live, end-to-end, with a one-click `/trace/$traceId` link rendering the matching Tempo timeline.

Follow-on issues land within the same wave but after the tracer:

- **RUSAA-83 remainder** — `get_callers`, `get_callees`, `get_trait_impls`, `run_query` MCP tools.
- **RUSAA-84 remainder** — session list / cancel / replay UI surface; cost circuit-breaker; reaper task.
- **RUSAA-85 remainder** — three-pane layout polish, history virtualization, session metadata pane, error states.

If horizontal-only slicing emerges as necessary mid-wave (e.g. an LLM-provider abstraction touches every tool), it MUST be re-justified in writing per COMPANY.md § Issue Hygiene Rules.

---

## 10. Test strategy

### 10.1 Unit (per crate)

- `crates/rb-mcp` — JSON-RPC envelope round-trips, `tools/list` schema matches the discovered tool surface, error code mapping is exhaustive, Streamable HTTP framing handles partial-chunk disconnect.
- `services/control-api` (existing test layout) — auth scope checks, tenant-drift rejection, budget enforcement, idle reaper, audit invariant (§7.4), partition-rollover degradation path.

### 10.2 Integration (`tests/`)

- `agent_session_full_lifecycle` — pending → running → completed; assert `(events count, audit count, span count)` triple matches.
- `agent_tenant_isolation_under_drift` — start session as tenant A, switch user to tenant B mid-session, assert next `tools/call` returns `tenant_drift`.
- `agent_budget_exceeded_input_tokens` — assert budget hard-fail emits the right event and DB row.
- `agent_idle_timeout_reaper` — assert reaper marks session `failed{error_kind:"idle_timeout"}` after `AGENT_IDLE_TIMEOUT_SECONDS`.
- `mcp_run_query_admin_only` — agent-scope key receives `insufficient_scope` style MCP error; admin scope succeeds.
- `mcp_unsupported_method` — `prompts/list` returns proper JSON-RPC `method_not_found`.
- `mcp_session_id_binding` — `Mcp-Session-Id` issued at `initialize` is required for subsequent `tools/call`; missing or stale rejects with structured error.
- `agent_audit_invariant` — generates 50 random sessions with varied tool calls, asserts §7.4 invariant for each.
- `agent_sse_reconnect_replays_no_gap` — kill + reconnect mid-session; assert continuity via `Last-Event-ID`.
- `agent_runtime_kind_immutable` — attempt to PATCH `runtime_kind` on a running session via direct DB UPDATE; assert no app endpoint allows it and trigger / app guard reject.
- `agent_oauth_required_when_no_token` — start `claude_code` session for a user with no `oauth_tokens` row; assert 401 `oauth_required`.
- `agent_oauth_refresh_renews_silently` — fixture an `oauth_tokens` row with access token expiring in 30s; tool call mid-session triggers refresh; assert no `session_failed` and `agent_oauth_claude_refresh_total{outcome="success"}` increments.
- `agent_litellm_unreachable_isolation` — point `LITELLM_BASE_URL` at a 502-only stub; assert `runtime_kind="opencode"` session creation returns 503 `llm_unavailable` while `runtime_kind="claude_code"` succeeds.
- `agent_litellm_virtual_key_lazy_creation` — first `runtime_kind="opencode"` session for a tenant triggers LiteLLM admin call to create the team + key; second session reuses cached key id (within TTL).

### 10.3 E2E (`frontend/tests/`, Playwright)

- `agent_session_tracer_e2e` — exact tracer flow above; one passing run is the wave acceptance gate.

### 10.4 Soak / load (manual, pre-ship)

One 6-hour soak: 25 active sessions, 1 tool call / 30s each, single tenant. Assert no leaked sessions, memory stable, audit invariant holds end-to-end.

---

## 11. Open questions for the board

Each has a working assumption that lets the wave ship without the answer.

### 11.1 LLM provider strategy — DECIDED (rev 3)

**Status:** Closed by board directive (CTO comment `ce3d7c11`, 2026-05-06). Architecture is documented in §3.4 (LiteLLM placement), §6.4 (per-runtime dispatch), §7.5 (OAuth + virtual-key scoping), §8.1 (env vars).

**Decision (board):**

1. **LiteLLM** is the unified LLM gateway/proxy for all non-OAuth provider calls.
2. **Two Rustacean agent runtimes for Wave 7** — `claude_code` and `opencode` — mirroring the first two slots of Paperclip's multi-runtime model. (Rev 3 originally specified three runtimes; rev 5 / RUSAA-895 deferred the `pi` slot — see §3.5 for the binary-identity, MCP-absence, and host-tool-sandbox findings and the explicit re-opening conditions.)
3. **Claude Code** uses the user's own OAuth token against their Claude Max plan; **no API-key spend on Rustacean's books**.
4. **OpenCode** routes through LiteLLM with per-tenant virtual keys (API-key-backed, tenant-scoped budgets).

**Why it stuck:** keeps Claude usage on the user's quota (cost containment + better incident isolation for the highest-traffic runtime), gives a single LiteLLM control plane for the API-keyed runtime, and aligns Rustacean's runtime taxonomy with Paperclip so internal users learn one mental model. Trade-offs (one new external service, an OAuth flow, a token-storage table) are itemised in §3.4 / §4.4 / §7.5.

**What rev 3 closes:** the rev 1/2 working assumption ("Anthropic-only with a small `LlmProvider` trait") is **superseded**. The trait shape lives now in `crates/rb-agent-runtime` as `AgentRuntime`, with two Wave-7 implementors at the runtime level, not the provider level (rev 5; rev 3 had three before pi was deferred — see §3.5). Provider abstraction (Anthropic vs OpenAI vs Bedrock) lives inside LiteLLM, not Rustacean code.

### 11.2 Frontend MCP client — proxied or direct

**Question:** Does the `/agents/*` UI talk to `/mcp` directly from the browser, or does the browser only call `/v1/agents/*` REST and the server-side runtime drives MCP?

**Working assumption:** **Server-side only.** Browser never sees an `/mcp` payload; it only calls REST + reads SSE. This keeps the API key server-side and makes the MCP surface a true server-to-server (or CLI-to-server) protocol. A Phase-8 "browser-as-MCP-client" path is feasible but out-of-scope here.

**Why it can wait:** The browser experience is identical either way; the back-end already plans to be the MCP host.

### 11.3 Conversation memory across sessions

**Question:** Do sessions share any state — past chat history, learned tool preferences — or is each session fully independent?

**Working assumption:** **Fully independent.** Each session is one prompt → one final answer; no cross-session memory in Wave 7. Phase 7 introduces "conversations" as a higher-level abstraction over multiple sessions if user demand emerges.

**Why it can wait:** Independent sessions are a strict subset; we can extend.

---

## 12. Risks

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| LLM provider quota exhausted by a single tenant | Medium | High | Per-tenant cost circuit-breaker (§7.3); per-session budgets. |
| MCP spec evolves (the protocol is young) and breaks our endpoint | Medium | Medium | Pin the protocol version in `initialize` response; add a version negotiation step in Phase 2. |
| Per-session tokio task leak under cancel-during-tool-call | Low | High | `JoinHandle::abort` on cancel; reaper sweeps any session with `last_event_at < NOW() - idle` regardless of status. Test 10.2 `agent_idle_timeout_reaper` covers it. |
| `agent_events` table grows unboundedly if partition rollover cron fails | Low | Medium | Alert `AgentEventsPartitionLag` (defined in §8.3) probes 7 days ahead so a missed monthly rollover is visible long before inserts would fail. Partitions are MONTH-sized so a 1-day alert lead is generous. |
| Long-lived SSE connections + control-api restart = mass reconnect storm | Medium | Medium | Existing `rb-sse` jittered reconnect (Wave-4 pattern); load test: simulate 250 reconnects in 5s. |
| MCP tool call leaks code from a different tenant via subtle Cypher | Low | Critical | `run_query` already routes through `TenantGraph::run` with AST injection + EXPLAIN-plan write check (ADR-008 §3.5). Wave 7 adds the audit invariant test. |
| Cost-tracking drift (we under-count tokens, tenant under-billed) | Medium | Low | Provider response IS authoritative; we record what the SDK returns. Reconcile against provider invoice quarterly. |
| Browser EventSource auth doesn't carry custom headers, so we can't pass an API-key to SSE | High (already known) | Medium | Same path as REQ-FE-08: SSE endpoint accepts session cookie; API-key clients use `curl`/programmatic clients which DO send custom headers. |

---

## 13. Documentation deliverables

- This ADR (`docs/decisions/ADR-009-agent-execution-architecture.md`).
- Updated `docs/architecture.md` (§"Agent Execution") — new diagram showing browser → control-api `/v1/agents/*` → in-process `AgentRegistry` → `rb-agent-runtime` adapter (one of `claude_code` / `opencode`) → either Anthropic (OAuth) or LiteLLM → underlying provider → `rb-query` callback path; OTel arrow to Tempo.
- Updated `docs/api-reference.md` — new endpoints catalogued (`/v1/agents/*`, `/v1/auth/oauth/claude/*`, `/v1/auth/litellm/rotate`), error-code matrix extended (`oauth_required`, `runtime_disabled`).
- New `docs/runbook.md` § "Agent operations" — how to read alerts (`LiteLLMUnreachable`, `OAuthClaudeRefreshFailureRate`, `AgentTenantCostBreach`), how to look up a session by trace_id, how to rotate a tenant's LiteLLM virtual key, how to revoke a user's Claude OAuth, how to drain sessions for a deploy.
- New `docs/deployment.md` § "LiteLLM" — Helm values, secret wiring, scaling guidance, the failure-mode matrix from §6.4 reproduced for ops.
- Updated `README.md` — one-paragraph description of agent execution + the two Wave-7 runtimes (`claude_code`, `opencode`); pointer here.

### 13.1 OAuth KMS key rotation runbook (RUSAA-862)

**Cadence:** Rotate `oauth-claude-v1` every **90 days** (calendar reminder: `RB_OAUTH_ENCRYPT_KEY_ID`
rotations align with Q1/Q2/Q3/Q4 starts).  A compromised key affects all stored OAuth refresh tokens
until the next rotation.

**Encryption scheme:**
- Algorithm: AES-256-GCM with a freshly generated 96-bit nonce per encryption call.
- Key derivation: HKDF-SHA-256(IKM=master_key, salt=user_id, info=key_id) → per-user 32-byte subkey.
  Even a leaked ciphertext blob is useless without both the master key and the user's UUID.
- Ciphertext format in `oauth_tokens.access_token` / `refresh_token`:
  `"v1:<base64(12-byte-nonce || aes-gcm-ciphertext)>"`.
  Plaintext rows (pre-migration or dev-mode) contain no `v1:` prefix.
- Key version tracked in `oauth_tokens.encryption_key_id` column (migration 012).

**Step-by-step rotation procedure:**

1. **Generate new key material**

   ```bash
   openssl rand -hex 32   # → new 64-char hex key, e.g. "a1b2c3..."
   ```

2. **Update secrets** (Kubernetes Secret or `rb-secrets` store):

   | Env var | Value |
   |---------|-------|
   | `RB_OAUTH_ENCRYPT_KEY` | `<new 64-char hex key>` |
   | `RB_OAUTH_ENCRYPT_KEY_ID` | `oauth-claude-v2` (or next version label) |
   | `RB_OAUTH_ENCRYPT_KEY_PREV` | `<old hex key>` |
   | `RB_OAUTH_ENCRYPT_KEY_PREV_ID` | `oauth-claude-v1` (the outgoing label) |
   | `RB_OAUTH_ROTATE_KEYS_ON_BOOT` | `true` |

3. **Deploy the new control-api pod(s).**  On startup, the service:
   - Logs `OAuth token cipher initialised key_id=oauth-claude-v2`.
   - Spawns the background rotation sweep which queries
     `SELECT … FROM agents.oauth_tokens WHERE encryption_key_id != 'oauth-claude-v2'`
     in batches of 50 rows, re-encrypting each row with the new key.
   - Emits `oauth_key_rotation_rows_rotated_total` and `oauth_key_rotation_rows_failed_total`
     Prometheus counters as the sweep progresses.
   - Logs `token_key_rotation: sweep finished ok=N errors=0` when complete.

4. **Verify** that all rows are on the new key:

   ```sql
   -- Should return exactly one row with the new key id and errors=0.
   SELECT encryption_key_id, COUNT(*) AS n
   FROM agents.oauth_tokens
   GROUP BY 1
   ORDER BY 1;
   ```

   Also check Prometheus: `oauth_key_rotation_rows_failed_total` should be 0.

5. **Remove the previous key** once verification passes:
   - Unset `RB_OAUTH_ENCRYPT_KEY_PREV`, `RB_OAUTH_ENCRYPT_KEY_PREV_ID`,
     and `RB_OAUTH_ROTATE_KEYS_ON_BOOT` from the secrets store.
   - Redeploy.  The service will log `RB_OAUTH_ENCRYPT_KEY_PREV is not set` at
     startup (informational only).

6. **Archive the retired key** in the organisation's key escrow with a
   destruction date of `today + 180 days` (double the rotation period, to allow
   incident-driven rollback).

**Rollback (key compromise):**
- Immediately set `RB_OAUTH_ENCRYPT_KEY` to a freshly generated key and `RB_OAUTH_ROTATE_KEYS_ON_BOOT=true`.
- Treat the previous key as compromised: revoke all Claude OAuth tokens affected via
  `DELETE /v1/auth/oauth/claude` per user (or bulk DELETE on `agents.oauth_tokens`) so
  that sessions using the old access token fail at the next refresh rather than silently
  continuing.
- Rotate LiteLLM virtual keys as a precaution (admin endpoint `POST /v1/auth/litellm/rotate`).
- File a P0 incident; follow the Security Incident Response runbook.

---

## 14. Acceptance criteria (mirrors RUSAA-719)

1. ADR doc lives at `docs/decisions/ADR-009-agent-execution-architecture.md`. ✅
2. All 6 areas addressed:
   - MCP protocol integration → §6.2, §1 (substrate), §2 (decision summary).
   - Session lifecycle → §4.1, §6.1, §10.2.
   - New service vs. control-api extension → §3 (extension), §3.4 (LiteLLM placement — the one new external service).
   - Event streaming → §5, §6.1.
   - Trace capture → §8.4, §6.1, §4.1 (`trace_id` column).
   - Security model → §7 in full, including §7.5 OAuth + LiteLLM virtual-key scoping (rev 3).
3. Board directive on LLM provider strategy (rev 3) absorbed: §3.4, §4.4, §6.4, §7.5, §8.1.
4. Board approval comment on the plan-document revision posted by an Architect or board member.

---

## 15. Forward-looking (out-of-scope but noted)

- **`pi` runtime evaluation — RESOLVED (rev 5, RUSAA-895).** See §3.5. Pi resolves to `@mariozechner/pi-coding-agent` (Node CLI). It has no MCP client and ships built-in `bash`/`edit`/`write` tools that bypass tenant scoping; both are blockers for Wave 7. The `pi` slot is closed for Wave 7 with explicit re-opening conditions documented in §3.5 (pi-side MCP support, or a sandboxed-subprocess host on our side, or a product decision to accept pi's tool surface as the agent surface).
- **Phase-2 binary split.** Triggers in §3.2; mechanics already pre-engineered (registry + routes + runtime adapters movable wholesale; LiteLLM stays put).
- **`code_embeddings` (pgvector) cleanup.** Inherited from ADR-008 §15; not a Wave-7 deliverable.
- **MCP `prompts/*` and `resources/*`.** Add when an external MCP client (Claude Code, IDE extension) demands them.
- **Browser-side MCP client.** Phase 8; depends on a browser-safe credentials story (PKCE-style flow or short-lived JWT minted by `/v1/agents/sessions`).
- **Agent-authored writes** (open PR, comment on a Linear issue, patch a doc). Needs a separate ADR with a complete trust + approval model; explicitly out of Wave 7.
- **Cross-session conversation memory.** §11.3.
- **Multi-agent orchestration** — agent-to-agent messages, supervisor agents, etc. Not on the Phase-6 roadmap.
- **`rb-mcp-cli`** — a small CLI to drive the MCP server for local development / debugging. Two-day task; deferred.
- **Additional runtime adapters** — `pi_local`, `codex_local`, `cursor_local`, `gemini_local`, `openclaw_gateway` (all present in Paperclip). The `AgentRuntime` trait shape supports them; we add only when product demand surfaces, and only after the LiteLLM model-list expansion (or, for the local-CLI variants, the sandbox-host work) that they would require. `pi_local` specifically: see §3.5 re-opening conditions.
- **OAuth providers other than Claude** — GitHub (for write-back), Google (Vertex), etc. The `oauth_tokens.provider` enum is widenable; not a Wave-7 deliverable.
- **LiteLLM HA across regions / multi-cluster.** Wave 7 ships single-region two-replica LiteLLM. Multi-region LiteLLM federation is a separate ADR if/when Rustacean goes multi-region.
- **Token rotation jobs for `oauth_tokens.encryption_key_id`.** AEAD scheme supports versioned KMS keys; the rotation job itself ships when KMS rotation policy lands org-wide.

---

## 16. Self-grilling pass (mandatory under COMPANY.md § Gate 1)

Recorded inline; the board may use this as the contested-questions log without re-grilling.

> **Grill — "Why not split into a new binary on day one?"** Because the capacity model (§3.1) puts us at ≤ 250 active sessions per region with ≤ 1.25 GB extra heap on the existing replica's 4 GB envelope. Splitting today buys isolation we don't need yet and doubles the surface for one heartbeat-cycle of value. The split shape is pre-engineered (§3.2) so doing it later costs one structural-refactor PR, not an architecture rewrite.
>
> **Grill — "If MCP spec is immature, why pick it now over a custom protocol?"** Because the only consumers Wave 7 plans to support — Claude Code, IDE extensions, future internal tools — already speak MCP. Inventing a custom protocol creates a one-off integration burden for every consumer. We pin the spec version in `initialize` (§12 risk row) so spec churn is detectable.
>
> **Grill — "SSE vs. WebSocket — really?"** Yes. Agent events are unidirectional. We already run `rb-sse` in production with reconnect + ring buffer + audit. Adding WebSocket = new dep, new auth path (CSRF), new infra in proxies, all for zero functional gain. (The "WebSocket has lower per-event overhead" claim is true at >1000 events/sec/session; we expect ≤ 10/sec/session.)
>
> **Grill — "Tenant_id in `agent_events` is denormalized — what if it drifts from `agent_sessions.tenant_id`?"** It can't drift in normal flow because it's stamped from `agent_sessions.tenant_id` at insert time and `agent_sessions.tenant_id` is `IMMUTABLE` (Postgres-level, NEW: see §4.1 — *enforce via column-level trigger that errors on UPDATE OF tenant_id*). Adding the trigger is two lines in the migration; making it explicit here.
>
> **Grill — "Token budget is a runtime check — what if the LLM provider already over-served us?"** Then we count what it served, charge the tenant for what we received, fail the session at budget exceeded, and don't make the next call. The budget IS a hard ceiling on future requests, not a refund mechanism. Acceptable; documented in §7.3.
>
> **Grill — "Why store full `input_prompt` in DB? PII risk."** Capped 64 KiB; behind tenant-scoped reads only; same retention as audit (90d). If a tenant marks data as sensitive, they can request opt-out (Phase 8 — privacy-by-policy). Today's posture is "audit log + agent log have identical retention and access control."
>
> **Grill — "What stops the agent from issuing 1000 sequential `tools/call` and exhausting wall-clock?"** Per-session wall-clock cap (§7.3) plus per-tool latency observability (§8.2 `agent_tool_call_duration_seconds`). If we see a spike of long-tail tool calls, alert fires before quota damage.
>
> **Grill — "If `agent_runtime` becomes a separate binary later, do MCP `Mcp-Session-Id` bindings survive process restarts?"** No — by design, sessions are restart-fragile in Phase 1; restart kills active sessions (`session_failed{error_kind:"internal"}`). Phase-2 split adds session affinity at the proxy layer (sticky session_id → agent-runtime replica); not a Wave-7 problem.
>
> **Grill — "Does `agent` scope let me also call `/v1/items/*` data-plane endpoints?"** No — `Agent` scope is disjoint from `Read`/`Write`/`Admin` (§7.2). A token with only `[Agent]` cannot read items via REST; it can only call MCP tools. That's the whole point — agent tooling and direct data-plane reads are separately auditable.
>
> **Grill — "What's the failure mode if Tempo is down when the agent UI tries to render `/trace/$traceId`?"** Same as REQ-FE-08 (ADR-008 §3.x): falls back to the `pipeline_stage_runs` + `audit_events` timeline. Agent sessions write to `audit_events`, so the fallback timeline shows tool-call rows even with Tempo dark.
>
> **Grill — "Why publish `cost_usd_micro` (custom OTel attribute) — won't downstream tools choke?"** It's namespaced under our own attribute; OTel collectors pass through unknown attrs. Tempo / Grafana renders it fine. If OTel adopts a standard `gen_ai.cost.usd` attribute later, we add an aliased emission for one release, then drop the custom one. Reversible.
>
> **Grill (rev 3) — "Why LiteLLM at all rather than calling provider SDKs directly?"** Three reasons. **(1)** Adding a new provider becomes a LiteLLM config edit, not a Rustacean code change — the cost of expanding to OpenAI / Bedrock / Vertex is essentially zero, while bringing along three different SDKs in-process would each need their own retry/timeouts/auth handling. **(2)** Provider credentials live entirely in LiteLLM, never in `control-api` — that is the chokepoint that makes credential rotation a pod restart, not a Rustacean release. **(3)** Per-tenant virtual keys with budget caps, request logging, and rate limits are LiteLLM-native; reimplementing them per-provider in Rustacean would replicate ~600 lines of code three times. The downside (one new external service) is real but bounded: §3.4 makes it shared, two-replica, with a defined unreachability story.
>
> **Grill (rev 3) — "Why does Claude Code bypass LiteLLM? Isn't that an inconsistency?"** It is — *deliberately*. Claude Code's value is "you pay nothing extra; your Max plan covers it." Routing it through LiteLLM would put a Rustacean-owned API key in the path, defeating that property. The OAuth path means the user authenticates with Anthropic directly, the access token is bound to their Max subscription, and Anthropic — not Rustacean's LiteLLM virtual key — eats the request cost. The "inconsistency" is the price of getting that property; we mitigate by making `runtime_kind` an explicit first-class field on every session row, every event, and every metric so the divergence is observable rather than hidden.
>
> **Grill (rev 3) — "What stops a malicious user from exfiltrating their stored OAuth refresh token?"** Two layers. **At rest:** AEAD with a per-row nonce, a `rb-secrets`-rotatable KMS key, and HKDF-derivation keyed by `user_id` so even if the ciphertext blob leaks, a per-user key is needed to decrypt. **In transit:** the plaintext is only ever in the runtime adapter's memory for the lifetime of one session; never sent to the browser, never logged, never written to `agent_events`. The remaining attack surface is "compromised control-api process memory" which already implies the attacker has every API key in the system; we are not trying to mitigate that scenario in Wave 7.
>
> **Grill (rev 3) — "If the LiteLLM admin API is compromised, can the attacker exfiltrate cross-tenant data via reading another tenant's virtual key?"** Worst case the attacker can issue arbitrary virtual keys and run requests, but they cannot read provider response bodies retroactively (LiteLLM logs are append-only and signed at the Loki layer). Our mitigations: `LITELLM_ADMIN_API_KEY` is one secret in `rb-secrets` with restricted access (Platform Engineer + CTO); rotation = secret rotation + LiteLLM pod restart. Detection: `mcp_requests_total` cross-checked against LiteLLM-side logs hourly; drift > 1% pages SRE. Not a zero-trust posture — documented; same risk class as today's database admin credentials.
>
> **Grill (rev 3) — "Three runtimes from day one — isn't that scope creep against COMPANY.md's vertical-slice rule?"** No, because the tracer-bullet (§9) ships **only** `runtime_kind="claude_code"` end-to-end. `OpenCodeRuntime` is in the wave but follows the tracer; it is a wave-internal follow-on, not future-wave material. The `AgentRuntime` trait is mandatory at tracer time, but only one implementor needs to be production-ready — that's exactly the vertical-slice posture (interface in place, second implementor is a per-issue follow-on). LiteLLM is deployed at tracer time but unexercised — the chart is in place so the OpenCode follow-on issue doesn't have to land infrastructure mid-wave. *(Rev 5 update: the original rev-3 grill referenced three runtimes including pi; pi has since been deferred — see §3.5.)*
>
> **Grill (rev 3) — "Why one virtual key per `(tenant, runtime_kind)` instead of one per session?"** Per-session keys would 100×-multiply key issuance traffic against LiteLLM and make hourly-budget enforcement harder (LiteLLM would need to aggregate up to the tenant level). Per-tenant-per-runtime keeps issuance bounded (≤ 2 keys per active tenant — one OpenCode, one Pi), aligns budgets with how the circuit-breaker is keyed (tenant_id), and still gives us per-runtime audit decomposition via the key metadata. Per-session traceability is preserved separately by `agent_sessions.id` flowing through OTel + audit logs.
