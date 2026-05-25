# Codemap: control-api

The main HTTP API service powering the rust-brain platform. Built on Axum 0.8, it serves the REST API, an MCP JSON-RPC endpoint, and SSE event streams for agent sessions. Stateless — all persistent state lives in PostgreSQL, Kafka, Neo4j, and Qdrant.

## Module tree

```
services/control-api/src/
├── main.rs                         # Binary entrypoint: init tracing, build state, start server
├── lib.rs                          # Crate root: re-exports Config, AppState, AppError, run()
├── config.rs                       # Env-based configuration (RB_*, KAFKA_*, OTEL_*, etc.)
├── error.rs                        # AppError enum → HTTP status mapping
├── server.rs                       # Axum server setup with tower middleware
├── state.rs                        # AppState, rate limiters, session caps, MCP store
├── openapi.rs                      # utoipa ApiDoc generation
├── crypto/
│   └── mod.rs                      # Token hashing (SHA-256)
├── middleware/
│   ├── mod.rs
│   ├── auth.rs                     # AuthContext extraction (session cookie / Bearer / anonymous)
│   ├── auth_tests.rs
│   ├── internal_auth.rs            # X-Internal-Secret header validation
│   ├── otel_trace.rs               # OpenTelemetry span injection
│   └── platform_admin.rs           # Platform admin role check
├── routes/
│   ├── mod.rs                      # build_public() + build_internal() router assembly
│   ├── auth.rs                     # POST /v1/auth/signup, /login
│   ├── auth_logout.rs              # POST /v1/auth/logout
│   ├── auth_verify.rs              # POST /v1/auth/verify-email, /resend-verification
│   ├── auth_password_reset.rs      # POST /v1/auth/forgot-password, /reset-password
│   ├── me.rs                       # GET /v1/me, POST /v1/me/switch-tenant
│   ├── api_keys.rs                 # /v1/api-keys CRUD
│   ├── health.rs                   # /health, /ready, /v1/_version, /health/build
│   ├── repos.rs                    # /v1/repos connect + list
│   ├── repos_tests.rs
│   ├── tenants/
│   │   ├── mod.rs
│   │   ├── delete.rs               # DELETE /v1/tenants/{id}
│   │   ├── members.rs              # /v1/tenants/{id}/members CRUD
│   │   └── role.rs                 # PUT /v1/tenants/{id}/members/{uid}/role
│   ├── github/
│   │   ├── mod.rs
│   │   ├── install.rs              # GET /v1/github/install-url, /callback
│   │   ├── repos.rs                # GET /v1/github/installations/{id}/available-repos
│   │   ├── webhook.rs              # POST /v1/github/webhook
│   │   └── health.rs               # GET /v1/health/github-app
│   ├── agents/
│   │   ├── mod.rs
│   │   ├── sessions.rs             # POST/GET /v1/agents/sessions
│   │   ├── sessions_db.rs          # Database queries for agent sessions
│   │   ├── sessions_tests.rs
│   │   ├── session_lifecycle.rs    # GET/DELETE /v1/agents/sessions/{id}
│   │   ├── session_queries.rs      # Session query helpers
│   │   ├── events/
│   │   │   ├── mod.rs
│   │   │   └── stream.rs           # GET /v1/agents/sessions/{id}/events (SSE)
│   │   ├── events_history.rs       # GET /v1/agents/sessions/{id}/events/history
│   │   ├── events_ingest.rs        # POST /internal/agent/sessions/{id}/events
│   │   └── events_ndjson.rs        # GET /v1/agents/sessions/{id}/log.ndjson
│   ├── mcp/
│   │   ├── mod.rs                  # POST /mcp (JSON-RPC 2.0 dispatch)
│   │   ├── dispatch.rs             # Method router: initialize, tools/list, tools/call
│   │   └── audit.rs                # MCP audit logging
│   ├── query/
│   │   ├── mod.rs
│   │   ├── items.rs                # GET /v1/repos/{repo_id}/items/{fqn_b64}
│   │   ├── search.rs               # POST /v1/search
│   │   ├── graph.rs                # POST /v1/graph/query
│   │   ├── traversal.rs            # callers + callees BFS
│   │   ├── impls.rs                # trait impl lookup
│   │   ├── usages.rs               # type usage lookup
│   │   └── modules.rs              # GET /v1/repos/{repo_id}/modules
│   ├── ingest/
│   │   ├── mod.rs
│   │   ├── trigger.rs              # POST /v1/repos/{repo_id}/ingestions
│   │   ├── events_stream.rs        # GET /v1/ingest/events (SSE)
│   │   ├── recent.rs               # GET /v1/ingestions/recent
│   │   ├── stages.rs               # GET /v1/ingestions/{id}/stages
│   │   └── test_publish.rs         # POST /v1/ingest/test-publish
│   ├── audit/
│   │   └── mod.rs                  # GET /v1/audit
│   └── admin/
│       ├── mod.rs
│       ├── github/
│       │   ├── mod.rs
│       │   ├── app_manifest.rs     # POST /v1/admin/github/app-manifest
│       │   ├── app_callback.rs     # GET /v1/admin/github/app-callback
│       │   └── app_status.rs       # GET /v1/admin/github/app-status
│       └── partition_maintenance.rs # POST /internal/admin/partition-maintenance
├── agents/
│   └── mod.rs                      # Agent execution helpers
├── ingest_consumer/
│   ├── mod.rs                      # Kafka consumer for ingestion pipeline events
│   ├── db.rs
│   └── sse.rs
├── jobs/
│   ├── mod.rs                      # Scheduled background jobs
│   └── tests.rs
└── bin/
    └── rb_test_producer.rs         # Dev tool for publishing test Kafka events
```

## Public route table

### Auth

| Method | Path | Handler |
|--------|------|---------|
| POST | `/v1/auth/signup` | `signup` |
| POST | `/v1/auth/login` | `login` |
| POST | `/v1/auth/logout` | `logout` |
| POST | `/v1/auth/verify-email` | `verify_email` |
| POST | `/v1/auth/resend-verification` | `resend_verification` |
| POST | `/v1/auth/forgot-password` | `forgot_password` |
| POST | `/v1/auth/reset-password` | `reset_password` |

### User / Tenants

| Method | Path | Handler |
|--------|------|---------|
| GET | `/v1/me` | `get_me` |
| POST | `/v1/me/switch-tenant` | `switch_tenant` |
| POST | `/v1/api-keys` | `create_api_key` |
| GET | `/v1/api-keys` | `list_api_keys` |
| DELETE | `/v1/api-keys/{id}` | `revoke_api_key` |
| DELETE | `/v1/tenants/{id}` | `delete_tenant` |
| GET | `/v1/tenants/{id}/members` | `list_members` |
| POST | `/v1/tenants/{id}/members` | `invite_member` |
| PUT | `/v1/tenants/{id}/members/{uid}/role` | `update_member_role` |
| DELETE | `/v1/tenants/{id}/members/{uid}` | `remove_member` |
| POST | `/v1/tenants/{id}/transfer-ownership` | `transfer_ownership` |

### Agents (Wave 7)

| Method | Path | Handler |
|--------|------|---------|
| POST | `/v1/agents/sessions` | `create_session` |
| GET | `/v1/agents/sessions` | `list_sessions` |
| GET | `/v1/agents/sessions/{id}` | `get_session` |
| DELETE | `/v1/agents/sessions/{id}` | `delete_session` |
| GET | `/v1/agents/sessions/{id}/events` | `session_events` (SSE) |
| GET | `/v1/agents/sessions/{id}/events/history` | `session_events_history` |
| GET | `/v1/agents/sessions/{id}/log.ndjson` | `session_log_ndjson` |

### MCP (Wave 7)

| Method | Path | Handler |
|--------|------|---------|
| POST | `/mcp` | `mcp_handler` (JSON-RPC 2.0) |

### GitHub

| Method | Path | Handler |
|--------|------|---------|
| GET | `/v1/github/install-url` | `github_install_url` |
| GET | `/v1/github/callback` | `github_callback` |
| POST | `/v1/github/webhook` | `github_webhook` |
| GET | `/v1/github/installations/{id}/available-repos` | `list_available_repos` |
| GET | `/v1/health/github-app` | `github_app_health` |

### Repos / Ingestion

| Method | Path | Handler |
|--------|------|---------|
| POST | `/v1/repos` | `connect_repo` |
| GET | `/v1/repos` | `list_repos` |
| POST | `/v1/repos/{id}/ingest` | `trigger_ingest` |
| POST | `/v1/repos/{repo_id}/ingestions` | `trigger_ingestion` |
| GET | `/v1/ingestions/recent` | `list_recent_runs` |
| GET | `/v1/ingestions/{id}/stages` | `get_stage_timeline` |
| GET | `/v1/ingest/events` | `events_stream` (SSE) |

### Query / Search / Graph

| Method | Path | Handler |
|--------|------|---------|
| GET | `/v1/repos/{repo_id}/items/{fqn_b64}` | `get_item` |
| GET | `/v1/repos/{repo_id}/items/{fqn_b64}/callers` | `get_callers` |
| GET | `/v1/repos/{repo_id}/items/{fqn_b64}/callees` | `get_callees` |
| GET | `/v1/repos/{repo_id}/items/{fqn_b64}/impls` | `get_trait_impls` |
| GET | `/v1/repos/{repo_id}/items/{fqn_b64}/usages` | `get_type_usages` |
| GET | `/v1/repos/{repo_id}/modules` | `get_module_tree` |
| POST | `/v1/search` | `search` |
| POST | `/v1/graph/query` | `post_graph_query` |

### Health

| Method | Path | Handler |
|--------|------|---------|
| GET | `/health` | `health_check` |
| GET | `/health/build` | `build_info` |
| GET | `/ready` | `ready_check` |
| GET | `/v1/_version` | `version` |
| GET | `/v1/health/consistency` | `consistency_check` |
| GET | `/v1/audit` | `list_audit_events` |

### Internal (X-Internal-Secret)

| Method | Path | Handler |
|--------|------|---------|
| PATCH | `/internal/agent/sessions/{id}/status` | `patch_session_status` |
| DELETE | `/internal/agent/sessions/{id}/api-key` | `delete_session_api_key` |
| POST | `/internal/agent/sessions/{id}/events` | `ingest_session_events` |
| POST | `/internal/admin/partition-maintenance` | `partition_maintenance` |

## Database role table

| Schema | Tables | Managed by |
|--------|--------|------------|
| `control` | `users`, `tenants`, `tenant_members`, `sessions`, `email_tokens`, `api_keys`, `auth_events`, `agent_sessions`, `agent_events` (partitioned), `oauth_tokens`, `github_app_config`, `github_installations`, `connected_repos`, `ingestion_runs`, `audit_events` | `migrate` service |
| `tenant_<uuid>` | `items`, `ingestion_stages` | Created atomically during signup |

## External dependencies (rb-* crates)

| Crate | Role |
|-------|------|
| `rb-auth` | Session/API-key auth, password hashing |
| `rb-build-info` | Compile-time build metadata |
| `rb-email` | Email templates and sender |
| `rb-github` | GitHub App OAuth, API client |
| `rb-kafka` | Kafka producer/consumer |
| `rb-kafka-health` | Kafka liveness probe |
| `rb-mcp` | MCP protocol types and handler dispatch |
| `rb-query` | Read-path queries (symbol lookup, traversal, search) |
| `rb-schemas` | Shared protobuf-generated types |
| `rb-secrets` | Zeroizing secret wrappers |
| `rb-sse` | Server-Sent Events helpers |
| `rb-storage-neo4j` | Neo4j graph driver |
| `rb-storage-pg` | PostgreSQL connection pool |
| `rb-storage-qdrant` | Qdrant vector store client |
| `rb-tenant` | TenantCtx and schema-name derivation |
| `rb-tracing` | OpenTelemetry integration |

## Related docs

- [Architecture overview](../architecture.md)
- [API reference](../api-reference.md)
- [ADR-009: Agent Execution Architecture](../decisions/ADR-009-agent-execution-architecture.md)
- [ADR-010: GitHub App tenant install](../decisions/ADR-010-github-app-tenant-install.md)
- [Runbook: auth/migrations sanity](../runbooks/auth-migrations-sanity.md)
- [Runbook: tenant provisioning](../runbooks/tenant-provisioning.md)
