# Architecture

## System overview

rust-brain is a multi-tenant platform built as a Rust monorepo with a React frontend. The backend is structured as a Cargo workspace with shared library crates and binary services. All infrastructure is managed with Docker Compose.

```
                          ┌──────────────────────────────┐
                          │          Browser             │
                          └──────────────┬───────────────┘
                                         │ HTTPS
                          ┌──────────────▼───────────────┐
                          │      Caddy (reverse proxy)   │
                          │  :80/:443 → :10080/:10443    │
                          └──────┬──────────┬────────────┘
                                 │          │
               ┌─────────────────▼─┐    ┌──▼───────────────┐
               │   control-api     │    │   frontend (dist) │
               │  Axum 0.8 :8080   │    │  Vite build       │
               │  + /mcp (JSON-RPC)│    └──────────────────-┘
               └──┬──┬──┬──┬──┬──┬┘
                  │  │  │  │  │  │
       ┌──────────▼┐ │  │  │  │  └──────────────┐
       │ PostgreSQL│ │  │  │  │   ┌─────────────▼──┐
       │ :5432     │ │  │  │  │   │ otel-collector  │
       └───────────┘ │  │  │  │   │ :4317/:4318     │
                     │  │  │  │   └───┬─────────────┘
          ┌──────────▼┐ │  │  │       │
          │ Neo4j     │ │  │  │  ┌────▼─────────────┐
          │ :7687 bolt│ │  │  │  │ Tempo / Prometheus│
          └───────────┘ │  │  │  │ Grafana dashboards│
                        │  │  │  └──────────────────-┘
             ┌──────────▼┐ │  │
             │ Qdrant    │ │  │
             │ :6333 REST│ │  │
             └───────────┘ │  │
                  ┌────────▼──┼──────────┐
                  │ Kafka (KRaft)        │──▶ ingestion pipeline
                  │ :9092 / :9094        │    (parse, extract, embed workers)
                  └──────┬───────────────┘
                         │
            ┌────────────▼──────┐    ┌──────────────────┐
            │ Ollama :11434     │    │  agent-runner     │
            │ (embeddings)      │    │  Kafka consumer   │
            └───────────────────┘    │  rb.agent.commands│
                                     └──┬──────────┬────┘
                                        │          │
                               ┌────────▼──┐ ┌────▼──────────────┐
                               │claude-login│ │ LiteLLM (external)│
                               │SSH sidecar │ │ (opencode runtime)│
                               │:12222      │ └──────────────────-┘
                               └────────────┘
                                     │
                              claude-credentials
                               (named volume)
```

### Agent execution topology (Wave 7)

The agent execution subsystem runs AI coding agents inside isolated workspaces. `control-api` exposes session management (`/v1/agents/sessions/*`) and an MCP server (`POST /mcp`). Session commands flow via Kafka to `agent-runner`, which spawns runtime-specific subprocesses.

```
Browser ──POST /v1/agents/sessions──▶ control-api
                                           │
                                     Kafka: rb.agent.commands
                                           │
                                     ┌─────▼──────────┐
                                     │  agent-runner   │
                                     │  (adapters)     │
                                     ├────────┬────────┤
                                     │claude  │opencode│
                                     │_code   │(lite   │
                                     │adapter │ llm)   │
                                     └───┬────┴────┬───┘
                                         │         │
                              claude-credentials   LITELLM_BASE_URL
                               (shared volume)     (external)
                                         │
                                     ┌───▼────────┐
                                     │claude-login │
                                     │SSH sidecar  │
                                     │(one-time    │
                                     │ /login)     │
                                     └─────────────┘
```

Runtime adapters: `ClaudeCodeAdapter` (OAuth via shared `claude-credentials` volume), `OpencodeAdapter` (LiteLLM proxy), `PiAdapter` (stub, ADR-009 Phase 3). See [ADR-009](decisions/ADR-009-agent-execution-architecture.md) for the full design.

---

## Workspace layout

The Cargo workspace (`Cargo.toml`) contains two kinds of members:

### Library crates (`crates/`)

| Crate | Purpose |
|-------|---------|
| `rb-auth` | Password hashing (argon2id), session tokens, API-key generation, in-memory rate limiter |
| `rb-email` | Email templates (minijinja) and transports: SMTP (lettre), console (dev), noop (tests) |
| `rb-schemas` | Protobuf schema definitions compiled by `prost-build` in `build.rs` |
| `rb-secrets` | Zeroizing wrapper types for sensitive string values (`zeroize`) |
| `rb-storage-pg` | PostgreSQL connection pool and repository abstractions (sqlx 0.8) |
| `rb-tenant` | `TenantId` newtype and schema-name derivation for per-tenant PostgreSQL schemas |
| `rb-tracing` | OpenTelemetry + tracing-subscriber initialisation, JSON log layer |
| `rb-query` | Read-path queries: symbol lookup, semantic search coordination, BFS call-graph traversal (callers/callees), trait-impl lookup, type-usage lookup |
| `rb-storage-neo4j` | Neo4j graph driver: tenant-isolated Cypher execution, write-operator detection, tenant label injection |
| `rb-storage-qdrant` | Qdrant vector store: tenant-isolated ANN search against the `rb_embeddings` collection |
| `rb-kafka` | Kafka producer/consumer helpers and topic management |
| `rb-github` | GitHub App authentication (JWT signing, installation tokens) and API client |
| `rb-blob` | Blob storage abstraction for large source artifacts |
| `rb-sse` | Server-Sent Events helpers for real-time ingestion status |
| `rb-parse-syn` | Rust source parser using `syn` for AST extraction |
| `rb-parse-tree-sitter` | Multi-language parser using tree-sitter for AST extraction |
| `rb-mcp` | MCP (Model Context Protocol) JSON-RPC types and handler dispatch |
| `rb-kafka-health` | Kafka liveness probe and consumer-lag tracking |
| `rb-build-info` | Compile-time build provenance (git SHA, timestamp, dirty flag) |
| `rb-feature-resolver` | Rust feature-flag resolution for conditional compilation |
| `rb-audit-cli` | CLI tool for querying the audit event log |

### Services (`services/`)

| Service | Binary | Purpose |
|---------|--------|---------|
| `control-api` | `control-api` | Main HTTP API — auth, tenants, API keys, user profile, GitHub integration, ingestion, code-symbol query, semantic search, graph traversal |
| `migrate` | `migrate` | Runs PostgreSQL migrations and Kafka topic creation |
| `ingest-clone` | `ingest-clone` | Stage 1 — clones Git repositories into a local working directory |
| `parse-worker` | `parse-worker` | Stage 2 — parses source files into AST items |
| `expand-worker` | `expand-worker` | Stage 3 — resolves macros and expands AST |
| `typecheck-worker` | `typecheck-worker` | Stage 4 — type-checks expanded AST |
| `ingest-graph` | `ingest-graph` | Stage 5 — extracts graph relations (calls, impls, usages) into Neo4j |
| `embed-worker` | `embed-worker` | Stage 6 — embeds code symbols via Ollama into Qdrant |
| `projector-pg` | `projector-pg` | Kafka → PostgreSQL projector for read-model materialization |
| `projector-neo4j` | `projector-neo4j` | Kafka → Neo4j projector for graph data |
| `tombstoner` | `tombstoner` | Async tenant deletion: drops PostgreSQL schemas, removes Neo4j nodes, deletes Qdrant points |
| `audit-worker` | `audit-worker` | Kafka → PostgreSQL projector for audit events |
| `agent-runner` | `rb-agent-runner` | Kafka consumer that spawns AI agent subprocesses (Claude Code, OpenCode) in isolated workspaces |

---

## Schema-per-tenant design

Each tenant gets its own PostgreSQL schema named `tenant_<uuid_hex>` (e.g. `tenant_a1b2c3`). This provides strong data isolation without the overhead of separate databases.

```
postgres database: rustbrain
├── schema: control           # shared control-plane tables
│   ├── users
│   ├── tenants
│   ├── tenant_members
│   ├── sessions
│   ├── email_tokens
│   ├── api_keys
│   └── auth_events
├── schema: tenant_<uuid_1>   # tenant 1 data (repos, etc.)
├── schema: tenant_<uuid_2>   # tenant 2 data
└── ...
```

The `control` schema is created by the `migrate` service on first run. Tenant schemas are created atomically during the signup transaction in `control-api`.

`TenantCtx` (in `rb-tenant`) derives the schema name from a `TenantId` and is the only place this derivation is allowed, keeping the mapping consistent.

---

## Service boundaries: control-api

`control-api` is a stateless HTTP service. It owns:

- **Auth surface** — signup, login, logout, email verification, password reset
- **Session management** — sliding-window `HttpOnly` sessions via `rb_session` cookie; session TTL configurable with `RB_SESSION_TTL_DAYS` (default 30 days)
- **API keys** — create, list, revoke; scopes: `read`, `write`, `admin`
- **Tenant membership** — invite, role update, remove, ownership transfer, tenant deletion
- **User profile** — `GET /v1/me` returns the caller's identity, current tenant, and all available tenants
- **Code intelligence** — semantic search (`POST /v1/search`), call-graph traversal (callers/callees), trait-impl lookup, type-usage lookup, raw Cypher queries (`POST /v1/graph/query`)
- **Health probes** — per-store liveness (`GET /health`) and Kafka consistency metrics (`GET /v1/health/consistency`)

The service has no internal state beyond the database connection pool and an in-memory rate limiter (`DashMap`). It can run multiple replicas behind a load balancer without shared state.

### Request lifecycle

```
HTTP request
  → Caddy (TLS termination)
  → control-api (Axum router)
      → tower middleware: request-id, CORS, tracing
      → auth middleware: extract rb_session cookie or Authorization: Bearer header
          → AuthContext::Session(SessionInfo) | ApiKey(ApiKeyInfo) | Anonymous | ExpiredSession
      → route handler
          → validate input
          → sqlx query (PostgreSQL)
          → return JSON response
  → OTLP traces → otel-collector → Tempo
```

### Auth middleware

`services/control-api/src/middleware/auth.rs` extracts the caller identity from every request:

- **Session cookie** (`rb_session`): SHA-256 hashes the token, looks up `control.sessions`, validates expiry.
- **Bearer token** (`Authorization: Bearer rb_...`): looks up `control.api_keys` by hash, records `last_used_at`.
- **Anonymous**: no credentials present.
- **ExpiredSession**: session found but past `expires_at`.

Handlers call `require_verified_session(auth)` or `require_session(auth)` to unwrap the correct variant or return a typed error.

---

## Auth flow

```
1. SIGNUP
   POST /v1/auth/signup
   ┌──────────────────────────────────────────────┐
   │ validate email format                        │
   │ validate password length (≥ 12)             │
   │ begin transaction:                           │
   │   check email not taken                     │
   │   insert control.tenants (new UUID)          │
   │   CREATE SCHEMA tenant_<uuid>               │
   │   insert control.users                      │
   │   insert control.tenant_members (role=owner)│
   │   insert control.email_tokens (kind=verify) │
   │   insert control.sessions                   │
   │   insert control.auth_events (event=signup) │
   │ commit                                       │
   │ send verification email (async, best-effort)│
   │ Set-Cookie: rb_session=<token>; HttpOnly    │
   └──────────────────────────────────────────────┘

2. VERIFY EMAIL
   POST /v1/auth/verify-email
   ┌──────────────────────────────────────────────┐
   │ SHA-256 hash the token                      │
   │ SELECT email_tokens FOR UPDATE              │
   │ check not used, not expired                 │
   │ UPDATE users SET email_verified_at = now()  │
   │ UPDATE email_tokens SET used_at = now()     │
   │ INSERT auth_events (event=email_verified)   │
   └──────────────────────────────────────────────┘

3. LOGIN
   POST /v1/auth/login
   ┌──────────────────────────────────────────────┐
   │ check rate limiter (5 failures / 10 min)    │
   │ fetch user + tenant + password_hash         │
   │ argon2id verify (constant-time on failure)  │
   │ check user status != suspended              │
   │ create new session (30-day sliding window)  │
   │ Set-Cookie: rb_session=<token>; HttpOnly    │
   └──────────────────────────────────────────────┘

4. SESSION USE
   Any authenticated request
   ┌──────────────────────────────────────────────┐
   │ auth middleware extracts rb_session cookie  │
   │ SHA-256 hash → lookup control.sessions      │
   │ if expired → AuthContext::ExpiredSession     │
   │ if valid → AuthContext::Session(SessionInfo) │
   │ GET /v1/me also fire-and-forgets:           │
   │   UPDATE sessions SET last_seen_at,         │
   │                        expires_at += TTL    │
   └──────────────────────────────────────────────┘
```

---

## API-first: OpenAPI as source of truth

Every handler in `control-api` is annotated with `#[utoipa::path(...)]`. The spec is generated — never hand-edited:

```bash
cargo run -p control-api -- print-openapi > openapi.json
```

CI enforces that `openapi.json` is always in sync with the handlers via `scripts/check-openapi-sync.sh`. The frontend generates TypeScript types from this spec:

```
control-api handlers
  ─── cargo build ──▶  print-openapi
                              │
                              ▼
                        openapi.json  ◀── CI sync check
                              │
                              ▼
                    openapi-typescript
                              │
                              ▼
               frontend/src/api/generated/schema.ts
                              │
                              ▼
                    frontend tsc -b  ◀── CI typecheck
```

Any type mismatch between the Rust handler and the TypeScript frontend is caught by CI before it can reach production.

---

## Read-side architecture (code intelligence)

The code intelligence query surface reads from three stores populated by the ingestion pipeline:

```
                              ┌────────────────┐
                              │  control-api   │
                              │  query routes  │
                              └──┬──┬──┬───────┘
                                 │  │  │
              ┌──────────────────▼┐ │  └──────────────┐
              │ PostgreSQL        │ │    ┌─────────────▼┐
              │ tenant_<uuid>     │ │    │   Qdrant     │
              │ (items table —    │ │    │ rb_embeddings│
              │  symbol lookup)   │ │    │ (ANN search) │
              └───────────────────┘ │    └──────────────┘
                                    │
                         ┌──────────▼──────────┐
                         │      Neo4j          │
                         │ (call graph, impls,  │
                         │  usages, type defs)  │
                         └─────────────────────┘
```

| Store | Crate | Query endpoints |
|-------|-------|-----------------|
| PostgreSQL | `rb-query` (pg module) | `GET /v1/repos/{id}/items/{fqn}` — symbol lookup by FQN |
| Qdrant | `rb-storage-qdrant` | `POST /v1/search` — semantic nearest-neighbour search |
| Neo4j | `rb-storage-neo4j` | callers, callees, impls, usages, `POST /v1/graph/query` |

**Tenant isolation** is enforced at every layer:
- **PostgreSQL** — per-tenant schema (`tenant_<uuid>`) qualified via `TenantCtx`
- **Qdrant** — mandatory `must` filter on `tenant_id` in every search query (`TenantVectorStore`)
- **Neo4j** — automatic tenant label injection into all Cypher node patterns (`TenantGraph`)

**Semantic search flow** (`POST /v1/search`):
1. Embed the user's natural-language query via Ollama (`RB_OLLAMA_URL`, model `RB_EMBEDDING_MODEL`)
2. Search the `rb_embeddings` Qdrant collection with the resulting vector, filtered by `tenant_id`
3. Return ranked results with cosine similarity scores

See [API reference](api-reference.md#search-endpoints) for request/response details.

---

## Observability

Every request is traced end-to-end with OpenTelemetry:

```
control-api  ──OTLP gRPC──▶  otel-collector  ──▶  Tempo (traces)
                                             ──▶  Prometheus (metrics)
                                                       │
                                                  Grafana dashboards
```

The `rb-tracing` crate initialises a `tracing-subscriber` stack with:
- JSON log formatting (for log aggregation)
- OpenTelemetry trace export via `opentelemetry-otlp`
- `tracing-opentelemetry` bridge connecting `tracing` spans to OTEL

`RUST_LOG` controls log verbosity (e.g. `info,control_api=debug`).  
`OTEL_SERVICE_NAME` sets the service name in traces (default: `control-api`).  
`OTEL_EXPORTER_OTLP_ENDPOINT` points to the collector (default in compose: `http://otel-collector:4317`).

---

## Technology choices

| Concern | Choice | Rationale |
|---------|--------|-----------|
| HTTP framework | Axum 0.8 | Ergonomic, tower-native, strong async story |
| Database | PostgreSQL 16 via sqlx 0.8 | Compile-time checked queries, no ORM overhead |
| Password hashing | argon2id (argon2 crate) | Current OWASP recommended algorithm |
| Async runtime | tokio | De-facto standard for Rust async |
| OpenAPI | utoipa 5 | Code-first, macro-based, good axum integration |
| Message bus | Kafka (KRaft, no ZooKeeper) | Durable, partitioned, replayable event log |
| Vector store | Qdrant | Fast approximate nearest-neighbor for embeddings |
| Graph store | Neo4j | Code knowledge graph for future code-intel features |
| LLM inference | Ollama | Local model serving for AI features |
| Frontend | React 18 + Vite + Tailwind + shadcn/ui | Fast DX, type-safe, accessible components |

---

## Decision records

Architecture decisions are captured as ADRs in [`docs/decisions/`](decisions/).

| ADR | Title | Status |
|-----|-------|--------|
| [ADR-009](decisions/ADR-009-agent-execution-architecture.md) | Agent Execution Architecture | Accepted (rev 6) — MCP server, agent session lifecycle, runtime adapters (Claude Code, OpenCode, Pi), LiteLLM gateway, per-tenant rate limits |
| [ADR-010](decisions/ADR-010-github-app-tenant-install.md) | Tenant-scoped GitHub App install + orphan-reclaim | Accepted — self-healing atomic CTE reclaim for cross-tenant installation collisions |
| [ADR-011](decisions/ADR-011-dev-stack-auto-rebuild.md) | Dev-stack auto-rebuild watcher | Accepted — selective per-path rebuilds via post-merge git hook, user systemd service, build-SHA provenance |
