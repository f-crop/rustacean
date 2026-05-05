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
               └──┬──────────┬─────┘    └──────────────────-┘
                  │          │
       ┌──────────▼──┐  ┌───▼──────────┐
       │  PostgreSQL  │  │ otel-collector│
       │  :5432       │  │ :4317/:4318  │
       └─────────────-┘  └───┬──────────┘
                              │
                    ┌─────────▼──────────┐
                    │  Tempo / Prometheus │
                    │  Grafana dashboards │
                    └────────────────────┘

Additional infra (not yet wired to control-api):
  Kafka (KRaft)  :9092 internal / :9094 external
  Neo4j          :7687 bolt
  Qdrant         :6333 REST
  Ollama         :11434
```

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
| `rb-query` | Read-path queries against tenant schemas: symbol lookup by `(repo_id, fqn)` via `TenantCtx::qualify` (ADR-008 §4.1) |

### Services (`services/`)

| Service | Binary | Purpose |
|---------|--------|---------|
| `control-api` | `control-api` | Main HTTP API — auth, tenants, API keys, user profile, GitHub integration, ingestion, code-symbol query |
| `migrate` | `migrate` | Runs PostgreSQL migrations and Kafka topic creation |

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
- **Tenant membership** — invite, role update, remove, ownership transfer
- **User profile** — `GET /v1/me` returns the caller's identity, current tenant, and all available tenants

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
