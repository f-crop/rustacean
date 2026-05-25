# API Reference

**Base URL**: `http://localhost:8080` (local) · `http://100.87.157.74:18080` (mars/Tailscale)

> **Source of truth**: [`openapi.json`](../openapi.json) (regenerated from code via `cargo run -p control-api -- print-openapi`) is the authoritative API contract. This document is a navigational index with prose explanations — when this page and the spec disagree, the spec wins.

**OpenAPI spec**: `GET /openapi.json` returns the full machine-readable spec. The frontend generates TypeScript types from this spec; do not hand-edit `openapi.json`.

**Authentication**: Most endpoints require an active session cookie (`rb_session`, `HttpOnly`) or a Bearer API key token.

---

## Environment variables

The control-api service reads all configuration from environment variables. None require a restart of other services; just restart the control-api container.

| Variable | Default | Required | Description |
|----------|---------|----------|-------------|
| `RB_DATABASE_URL` | — | **yes** | PostgreSQL connection string |
| `RB_LISTEN_ADDR` | `0.0.0.0:8080` | no | Address and port to bind |
| `RB_BASE_URL` | `http://localhost:8080` | no | Public base URL (used in email links) |
| `RB_CORS_ORIGINS` | `http://localhost:15173` | no | Comma-separated allowed CORS origins |
| `RB_SESSION_TTL_DAYS` | `30` | no | Sliding session expiry window in days |
| `RB_ARGON2_MEMORY_KB` | `19456` | no | Argon2id memory cost (KiB) |
| `RB_ARGON2_TIME_COST` | `2` | no | Argon2id iteration count |
| `RB_ARGON2_PARALLELISM` | `1` | no | Argon2id parallelism |
| `RB_EMAIL_TRANSPORT` | `console` | no | `console` (stdout), `smtp`, or `noop` |
| `RB_SECURE_COOKIES` | `true` | no | Set the `Secure` flag on `rb_session` cookies. Set to `false` when running behind an HTTP proxy in development. |
| `OTEL_SERVICE_NAME` | `control-api` | no | Service name in traces |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | — | no | OTLP gRPC endpoint (e.g. `http://otel-collector:4317`) |
| `RB_NEO4J_URI` | — | no | Bolt URI for Neo4j (e.g. `bolt://neo4j:7687`). Graph endpoints return 503 when absent. |
| `RB_NEO4J_USER` | `neo4j` | no | Neo4j username |
| `RB_NEO4J_PASSWORD` | — | no | Neo4j password |
| `KAFKA_BOOTSTRAP_SERVERS` | `kafka:9092` | no | Kafka broker list for the ingest consumer |
| `RB_QDRANT_URL` | — | no | Qdrant REST base URL (e.g. `http://qdrant:6333`). Search endpoint returns 503 when absent. |
| `RB_OLLAMA_URL` | — | no | Ollama HTTP base URL (e.g. `http://ollama:11434`). Search endpoint returns 503 when absent. |
| `RB_EMBEDDING_MODEL` | `nomic-embed-text` | no | Ollama model for query embedding. Must match the model used by `embed-worker`. |
| `RUST_LOG` | — | no | Log filter (e.g. `info,control_api=debug`) |

---

## Error response format

All error responses return JSON:

```json
{
  "error": "error_code_snake_case",
  "message": "Human-readable description"
}
```

Common error codes:

| Code | HTTP | Description |
|------|------|-------------|
| `invalid_email` | 400 | Email address fails format validation |
| `weak_password` | 400 | Password is shorter than 12 characters |
| `invalid_token` | 400 | Token is expired, already used, or not found |
| `invalid_input` | 400 | Request body missing required fields |
| `invalid_credentials` | 401 | Email/password combination is wrong |
| `unauthorized` | 401 | No valid session or API key presented |
| `session_expired` | 401 | Session token found but past `expires_at` |
| `email_not_verified` | 403 | Session exists but email has not been verified |
| `account_suspended` | 403 | User account has been suspended |
| `not_a_member` | 403 | Caller is not a member of the target tenant |
| `insufficient_role` | 403 | Caller lacks the required tenant role |
| `email_taken` | 409 | Email address is already registered |
| `already_member` | 409 | User is already a member of the tenant |
| `cannot_remove_owner` | 400 | Attempt to demote or remove the tenant owner |
| `rate_limited` | 429 | Login rate limit exceeded (5 failures / 10 min) |
| `insufficient_scope` | 403 | API key presented but lacks the required scope (e.g. `read`) |
| `insufficient_role` | 403 | Session user lacks the required tenant role (e.g. `admin` or `owner`) |
| `cypher_write_denied` | 400 | Cypher query contains write operators in read-only mode |
| `graph_not_configured` | 503 | Neo4j graph store not configured on this instance (`RB_NEO4J_URI` absent) |
| `confirmation_mismatch` | 400 | `X-Confirm` header value does not match tenant slug |
| `kafka_unavailable` | 503 | Kafka broker unreachable; request is safely retryable |

---

## Health endpoints

### GET /health

Liveness probe with per-store connectivity status. Always returns 200 (even when stores are degraded) so load balancers do not kill the process — inspect `status` for fine-grained health. Public / unauthenticated.

**Response 200**
```json
{
  "status": "ok",
  "stores": {
    "postgres": "ok",
    "neo4j": "ok",
    "qdrant": "ok",
    "kafka": "ok"
  }
}
```

| Field | Values | Description |
|-------|--------|-------------|
| `status` | `ok`, `degraded` | `ok` when all stores are reachable; `degraded` otherwise |
| `stores.postgres` | `ok`, `error` | PostgreSQL `SELECT 1` probe |
| `stores.neo4j` | `ok`, `error`, `unknown` | TCP connect to Neo4j bolt port; `unknown` when `RB_NEO4J_URI` not set |
| `stores.qdrant` | `ok`, `error`, `unknown` | Qdrant `/healthz` probe; `unknown` when `RB_QDRANT_URL` not set |
| `stores.kafka` | `ok`, `error`, `unknown` | Last Kafka event age; `unknown` when no event has ever been received |

### GET /ready

Readiness probe. Returns 200 when the service is ready to serve traffic (database connected).

**Response 200**
```json
{ "status": "ok" }
```

### GET /openapi.json

Returns the full OpenAPI 3.1 spec as JSON. Used by `npm run gen:api` to generate TypeScript types.

---

## Auth endpoints

### POST /v1/auth/signup

Register a new user and create their first tenant workspace.

- Creates a `control` user, a new tenant, a `tenant_<uuid>` PostgreSQL schema, and an owner membership — all in a single transaction.
- Sets an `HttpOnly` `rb_session` cookie on success.
- Sends a verification email (token valid 1 hour). In dev mode (`RB_EMAIL_TRANSPORT=console`) the link is printed to the API logs.

**Request**
```json
{
  "email": "alice@example.com",
  "password": "correct-horse-battery",
  "tenant_name": "Acme Corp"
}
```

| Field | Type | Rules |
|-------|------|-------|
| `email` | string | Must contain `@` and a dotted domain |
| `password` | string | Minimum 12 characters |
| `tenant_name` | string | Converted to URL slug; empty string falls back to `workspace` |

**Response 201** — user created, email verification required
```json
{
  "email_verification_required": true,
  "user_id": "550e8400-e29b-41d4-a716-446655440000"
}
```
Cookie: `rb_session=<token>; HttpOnly; SameSite=Lax; Path=/; Secure`

**Response 400** — `invalid_email` or `weak_password`  
**Response 409** — `email_taken`

---

### POST /v1/auth/verify-email

Consume a single-use email verification token.

**Request**
```json
{ "token": "<plaintext-token-from-email>" }
```

**Response 204** — email verified  
**Response 400** — `invalid_token` (expired, already used, or not found)

---

### POST /v1/auth/login

Authenticate with email and password, creating a new session.

- Verifies credentials with argon2id (constant-time on failure).
- Rate-limited: 5 failures per 10-minute window → 429 for 15 minutes.
- Sets a new `HttpOnly` `rb_session` cookie.

**Request**
```json
{
  "email": "alice@example.com",
  "password": "correct-horse-battery"
}
```

**Response 200**
```json
{
  "user_id": "550e8400-e29b-41d4-a716-446655440000",
  "tenant_id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
  "email_verification_required": false
}
```
Cookie: `rb_session=<token>; HttpOnly; SameSite=Lax; Path=/; Secure`

If `email_verification_required` is `true`, redirect the user to complete verification before allowing tenant access.

**Response 401** — `invalid_credentials`  
**Response 403** — `account_suspended`  
**Response 429** — `rate_limited`

---

### POST /v1/auth/logout

Revoke the current session. Clears the `rb_session` cookie.

**Auth required**: active session cookie

**Request**: empty body `{}`

**Response 204** — session revoked  
**Response 401** — `unauthorized`

---

### POST /v1/auth/forgot-password

Request a password-reset email. Always returns 200 to prevent email enumeration. When the email is found, a reset link with a **15-minute** expiry is emailed. When not found, a dummy argon2id hash is computed to keep response time indistinguishable.

**Request**
```json
{ "email": "alice@example.com" }
```

**Response 200** — always (regardless of whether email is registered)

---

### POST /v1/auth/reset-password

Consume a reset token and set a new password. All active sessions for the user are revoked — re-authentication is required.

**Request**
```json
{
  "token": "<plaintext-token-from-email>",
  "new_password": "new-correct-horse-battery"
}
```

**Response 204** — password updated, all sessions revoked  
**Response 400** — `invalid_token` or `weak_password`

---

## User profile endpoints

### GET /v1/me

Return the authenticated user's profile, current tenant, and all available tenants. As a side effect, refreshes the session's `last_seen_at` and extends `expires_at` by `RB_SESSION_TTL_DAYS` (sliding window).

**Auth required**: verified session (email must be verified)

**Response 200**
```json
{
  "user": {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "email": "alice@example.com",
    "status": "active",
    "email_verified": true,
    "created_at": "2026-04-01T12:00:00Z"
  },
  "current_tenant": {
    "id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
    "name": "Acme Corp",
    "slug": "acme-corp-a1b2c3",
    "role": "owner"
  },
  "available_tenants": [
    {
      "id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
      "name": "Acme Corp",
      "slug": "acme-corp-a1b2c3",
      "role": "owner"
    }
  ]
}
```

**Response 401** — `unauthorized` or `session_expired`  
**Response 403** — `email_not_verified`

---

### POST /v1/me/switch-tenant

Switch the active tenant for the current session. The caller must already be a member of the target tenant.

**Auth required**: verified session

**Request**
```json
{ "tenant_id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8" }
```

**Response 200**
```json
{
  "current_tenant": {
    "id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
    "name": "Second Workspace",
    "slug": "second-workspace-d4e5f6",
    "role": "admin"
  }
}
```

**Response 401** — `unauthorized`  
**Response 403** — `email_not_verified` or `not_a_member`  
**Response 404** — tenant not found or inactive

---

## API key endpoints

API keys allow machine-to-machine authentication. They are long-lived and do not use sessions. The plaintext key is returned exactly once at creation time.

**Key format**: `rb_live_<32hex>` (shown only on creation)

### POST /v1/api-keys

Create a new API key for the current session's tenant.

**Auth required**: active session (email verification not required)

**Request**
```json
{
  "name": "CI pipeline",
  "scopes": ["read", "write"]
}
```

| Scope | Description |
|-------|-------------|
| `read` | Read-only access to tenant resources |
| `write` | Create and update resources |
| `admin` | Full administrative access |

**Response 201**
```json
{
  "id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "key": "rb_live_<32-lowercase-hex-characters>",
  "name": "CI pipeline",
  "scopes": ["read", "write"],
  "created_at": "2026-04-26T10:00:00Z"
}
```

Store the `key` value securely — it cannot be retrieved after this response.

**Response 400** — empty name or empty scopes  
**Response 401** — `unauthorized`

---

### GET /v1/api-keys

List all active (non-revoked) API keys for the current session's tenant. Plaintext keys are never returned.

**Auth required**: active session

**Response 200**
```json
{
  "keys": [
    {
      "id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
      "name": "CI pipeline",
      "scopes": ["read", "write"],
      "last_used_at": "2026-04-25T08:30:00Z",
      "created_at": "2026-04-01T12:00:00Z"
    }
  ]
}
```

**Response 401** — `unauthorized`

---

### DELETE /v1/api-keys/{id}

Revoke an API key. Revocation is immediate and irreversible. Any member of the tenant can revoke any key belonging to that tenant.

**Auth required**: active session

**Path parameter**: `id` — UUID of the API key to revoke

**Response 204** — key revoked  
**Response 401** — `unauthorized`  
**Response 404** — key not found or already revoked

---

## Tenant membership endpoints

All tenant endpoints require an active session with a sufficient role in the target tenant.

Roles: `member` < `admin` < `owner`

### POST /v1/tenants/{id}/members

Invite a user to a tenant by email.

- If the user **already has an account**: they are added as `member` immediately (status 201).
- If the user **does not have an account**: an invite email with a signup link is sent (status 202).

**Auth required**: admin or owner role in tenant `{id}`

**Path parameter**: `id` — tenant UUID

**Request**
```json
{ "email": "bob@example.com" }
```

**Response 201** — existing user added directly
```json
{
  "invited": false,
  "user_id": "550e8400-e29b-41d4-a716-446655440000",
  "email": "bob@example.com",
  "role": "member"
}
```

**Response 202** — invite email sent
```json
{
  "invited": true,
  "user_id": null,
  "email": "bob@example.com",
  "role": "member"
}
```

**Response 401** — `unauthorized`  
**Response 403** — `not_a_member` or `insufficient_role`  
**Response 409** — `already_member`

---

### PUT /v1/tenants/{id}/members/{uid}/role

Change a member's role within a tenant.

- Cannot change the owner's role — use `transfer-ownership` instead.
- Cannot set `owner` as the new role — use `transfer-ownership` instead.

**Auth required**: admin or owner role in tenant `{id}`

**Path parameters**:
- `id` — tenant UUID  
- `uid` — user UUID of the member to update

**Request**
```json
{ "role": "admin" }
```

Valid roles for this endpoint: `member`, `admin`

**Response 200**
```json
{
  "user_id": "550e8400-e29b-41d4-a716-446655440000",
  "role": "admin"
}
```

**Response 400** — `cannot_remove_owner` or invalid role  
**Response 401** — `unauthorized`  
**Response 403** — `insufficient_role`  
**Response 404** — member not found

---

### DELETE /v1/tenants/{id}/members/{uid}

Remove a member from a tenant. Cannot remove the owner. The removed member's active sessions for this tenant are immediately revoked.

**Auth required**: admin or owner role in tenant `{id}`

**Path parameters**:
- `id` — tenant UUID  
- `uid` — user UUID of the member to remove

**Response 204** — member removed  
**Response 400** — `cannot_remove_owner`  
**Response 401** — `unauthorized`  
**Response 403** — `insufficient_role`  
**Response 404** — member not found

---

### POST /v1/tenants/{id}/transfer-ownership

Transfer ownership of a tenant to another existing member. Atomically:
1. Sets the current owner's role to `admin`.
2. Sets the target member's role to `owner`.

If `user_id` equals the caller's own ID, the operation is a no-op (returns 204 immediately).

**Auth required**: owner role in tenant `{id}`

**Path parameter**: `id` — tenant UUID

**Request**
```json
{ "user_id": "550e8400-e29b-41d4-a716-446655440000" }
```

**Response 204** — ownership transferred  
**Response 401** — `unauthorized`  
**Response 403** — `insufficient_role` (must be owner)  
**Response 404** — target user is not a member of the tenant

---

### DELETE /v1/tenants/{id}

Delete a tenant and all associated data. Soft-deletes the tenant (sets `status = 'deleting'`, `deleted_at = now()`), cancels all in-flight ingestion runs, and emits a tombstone to `rb.tombstones.v1`. The tombstoner service performs async cleanup across all three stores: PostgreSQL schema drop, Neo4j node removal, and Qdrant point deletion.

**Auth required**: owner role in tenant `{id}`

**Path parameter**: `id` — tenant UUID

**Required header**: `X-Confirm: <tenant_slug>` — must match the tenant slug exactly (case-insensitive). Prevents accidental deletions.

**Idempotent**: if the tenant is already in `deleting` or `deleted` state, returns `204 No Content` immediately.

**Request**
```
DELETE /v1/tenants/550e8400-e29b-41d4-a716-446655440000
X-Confirm: acme-corp
```

**Response 202** — deletion initiated
```json
{ "tenant_id": "550e8400-e29b-41d4-a716-446655440000", "status": "deleting" }
```

**Response 204** — tenant was already deleting/deleted (idempotent)  
**Response 400** — `confirmation_mismatch` (X-Confirm value did not match tenant slug)  
**Response 401** — `unauthorized`  
**Response 403** — `insufficient_role` (must be owner)  
**Response 404** — tenant not found  
**Response 503** — `kafka_unavailable` (Kafka broker unreachable; request is safely retryable)

---

## Authentication guide: using the API programmatically

### Session-based (browser / curl)

```bash
# 1. Log in and save the session cookie
curl -s -c cookies.txt -X POST http://localhost:8080/v1/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"email":"alice@example.com","password":"correct-horse-battery"}'

# 2. Use the cookie for subsequent requests
curl -s -b cookies.txt http://localhost:8080/v1/me | jq .
```

### API key-based (CI / scripts)

```bash
# 1. Create an API key (requires an active session)
KEY=$(curl -s -b cookies.txt -X POST http://localhost:8080/v1/api-keys \
  -H 'Content-Type: application/json' \
  -d '{"name":"CI","scopes":["read"]}' | jq -r .key)

# 2. Use the key as a Bearer token
curl -s -H "Authorization: Bearer $KEY" http://localhost:8080/v1/me | jq .
```

---

## Query endpoints

Query endpoints read from the ingested code graph stored in each tenant's PostgreSQL schema. They accept both session cookies and API keys with the `read` scope.

### GET /v1/repos/{repo_id}/items/{fqn_b64}

Retrieve a single code symbol by its fully-qualified name (FQN) within a repository (REQ-DP-02 / ADR-008 §12.2).

**Auth required**: verified session **or** API key with `read` scope

**Path parameters**:
- `repo_id` — UUID of the connected repository (from `GET /v1/repos`)
- `fqn_b64` — URL-safe base64 (no padding, RFC 4648 §5) encoded FQN

**Encoding the FQN**

```bash
# Shell one-liner (GNU coreutils)
FQN="my_crate::module::MyStruct"
FQN_B64=$(printf '%s' "$FQN" | base64 -w0 | tr '+/' '-_' | tr -d '=')
```

**Response 200** — symbol found
```json
{
  "id": "3fa85f64-5717-4562-b3fc-2c963f66afa6",
  "fqn": "my_crate::module::MyStruct",
  "kind": "STRUCT",
  "repo_id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
  "source_path": "src/module.rs",
  "line_start": 42,
  "line_end": 58
}
```

When the item's stored source exceeds the inline threshold, `blob_ref` is populated and `source_preview` is absent:

```json
{
  "id": "3fa85f64-5717-4562-b3fc-2c963f66afa6",
  "fqn": "my_crate::huge_fn",
  "kind": "FN",
  "repo_id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
  "source_path": "src/lib.rs",
  "line_start": 1,
  "line_end": 600,
  "blob_ref": "rb-blob://tenant_a1b2c3/items/3fa85f64.json"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `id` | UUID | Internal symbol identifier |
| `fqn` | string | Fully-qualified name as stored by the parse worker |
| `kind` | string | Symbol type: `FN`, `STRUCT`, `ENUM`, `TRAIT`, `IMPL`, `MOD`, `TYPE`, `CONST`, `STATIC`, `MACRO` |
| `repo_id` | UUID | Repository the symbol belongs to |
| `source_path` | string? | Repo-relative file path (e.g. `src/lib.rs`) |
| `line_start` | int? | 1-based start line |
| `line_end` | int? | 1-based end line |
| `source_preview` | string? | Inline source text (small items only; omitted when `blob_ref` is present) |
| `blob_ref` | string? | `rb-blob://` URI for full AST JSON (large items only; omitted otherwise) |

**Response 400** — `invalid_input` (malformed base64 or non-UTF-8 bytes)  
**Response 401** — `unauthorized` or `session_expired`  
**Response 403** — `email_not_verified` or `insufficient_scope`  
**Response 404** — repository not found, belongs to a different tenant, or FQN absent

Cross-tenant requests are always rejected with 404 — the caller's tenant must own `repo_id` (AC4).

**Example — curl with API key**

```bash
REPO_ID="6ba7b810-9dad-11d1-80b4-00c04fd430c8"
FQN="my_crate::module::MyStruct"
FQN_B64=$(printf '%s' "$FQN" | base64 -w0 | tr '+/' '-_' | tr -d '=')

curl -s \
  -H "Authorization: Bearer $KEY" \
  "http://localhost:8080/v1/repos/${REPO_ID}/items/${FQN_B64}" | jq .
```

**Example — curl with session cookie**

```bash
curl -s -b cookies.txt \
  "http://localhost:8080/v1/repos/${REPO_ID}/items/${FQN_B64}" | jq .
```

---

## Search endpoints

### POST /v1/search

Semantic code search across embedded code symbols within the caller's tenant. Embeds the query via Ollama, performs approximate nearest-neighbour search in the Qdrant `rb_embeddings` collection filtered by `tenant_id`, and returns ranked results.

Requires `RB_QDRANT_URL` and `RB_OLLAMA_URL` to be configured; returns 503 otherwise.

**Auth required**: verified session **or** API key with `read` scope

**Request**
```json
{
  "q": "function that handles authentication",
  "limit": 10,
  "filters": {
    "repo_id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8"
  }
}
```

| Field | Type | Rules |
|-------|------|-------|
| `q` | string | Natural-language query to embed and search. Must not be empty. |
| `limit` | int? | Maximum results to return (default 10, max 50) |
| `filters.repo_id` | UUID? | Restrict results to a single repository. Must belong to the caller's tenant. |

**Response 200**
```json
{
  "results": [
    {
      "fqn": "my_crate::auth::verify_token",
      "crate_name": "my_crate",
      "repo_id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
      "score": 0.92
    },
    {
      "fqn": "my_crate::middleware::extract_session",
      "crate_name": "my_crate",
      "repo_id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
      "score": 0.81
    }
  ]
}
```

| Field | Type | Description |
|-------|------|-------------|
| `results[].fqn` | string | Fully-qualified name (e.g. `my_crate::module::my_fn`) |
| `results[].crate_name` | string | Top-level crate name extracted from the FQN |
| `results[].repo_id` | string | Repository UUID this symbol belongs to |
| `results[].score` | float | Cosine similarity score in `[0, 1]` |

**Response 400** — `invalid_input` (empty query or limit out of range)
**Response 401** — `unauthorized` or `session_expired`
**Response 403** — `email_not_verified` or `insufficient_scope`
**Response 404** — `repo_id` filter specified but repository not found or belongs to another tenant
**Response 503** — Qdrant or Ollama not configured on this instance

**Example**

```bash
curl -s -b cookies.txt -X POST http://localhost:8080/v1/search \
  -H 'Content-Type: application/json' \
  -d '{"q":"function that handles authentication","limit":5}' | jq .
```

---

## Code graph traversal endpoints

These endpoints traverse the Neo4j code knowledge graph to discover call relationships, trait implementations, and type usages. All require `RB_NEO4J_URI` to be configured; they return 503 otherwise.

All traversal endpoints use URL-safe base64 encoding (no padding, RFC 4648 §5) for the `fqn_b64` path parameter — the same encoding used by [GET /v1/repos/{repo_id}/items/{fqn_b64}](#get-v1reposrepo_iditemsfqn_b64).

### GET /v1/repos/{repo_id}/items/{fqn_b64}/callers

List all functions that transitively call the target item. BFS traversal of `CALLS` and `CALL_INSTANTIATES` edges backward from the root. Cycle detection prevents infinite loops; per-edge provenance distinguishes static, monomorphized, and dynamic calls.

**Auth required**: verified session **or** API key with `read` scope

**Path parameters**:
- `repo_id` — UUID of the repository (must belong to the caller's tenant)
- `fqn_b64` — URL-safe base64 (no padding) encoded FQN of the target item

**Query parameters**:

| Parameter | Type | Default | Range | Description |
|-----------|------|---------|-------|-------------|
| `depth` | int | 3 | 1–10 | BFS traversal depth |
| `limit` | int | 50 | 1–200 | Maximum edges to return per page |
| `cursor` | string? | — | — | Opaque continuation cursor from a prior response |

**Response 200**
```json
{
  "root": {
    "fqn": "my_crate::module::my_fn",
    "name": "my_fn",
    "kind": "FN",
    "file_path": "src/module.rs",
    "line": 42
  },
  "nodes": [
    {
      "fqn": "my_crate::caller_a",
      "name": "caller_a",
      "kind": "FN",
      "file_path": "src/lib.rs",
      "line": 10
    }
  ],
  "edges": [
    {
      "from_fqn": "my_crate::caller_a",
      "to_fqn": "my_crate::module::my_fn",
      "depth": 1,
      "provenance": "direct"
    }
  ],
  "cycles_detected": false,
  "next_cursor": null
}
```

| Field | Type | Description |
|-------|------|-------------|
| `root` | object | The target item (BFS starting point) |
| `nodes[]` | array | Discovered nodes during BFS (excluding root) |
| `edges[]` | array | Directed edges in the call graph |
| `edges[].provenance` | string | `"direct"` — static call, `"monomorph"` — monomorphized generic, `"dyn_candidate"` — dynamic dispatch candidate |
| `cycles_detected` | bool | `true` when BFS encountered a cycle |
| `next_cursor` | string? | Pass to the next request for pagination; `null` when no more results |

Node fields (`root` and `nodes[]`): `fqn` (string), `name` (string?), `kind` (string?), `file_path` (string?), `line` (int?). Optional fields are omitted from the response when absent.

**Response 400** — `invalid_input` (malformed base64, depth > 10, or invalid cursor)
**Response 401** — `unauthorized` or `session_expired`
**Response 403** — `email_not_verified` or `insufficient_scope`
**Response 404** — repository not found or belongs to another tenant
**Response 503** — Neo4j graph not configured on this instance

**Example**

```bash
FQN="my_crate::module::my_fn"
FQN_B64=$(printf '%s' "$FQN" | base64 -w0 | tr '+/' '-_' | tr -d '=')
REPO_ID="6ba7b810-9dad-11d1-80b4-00c04fd430c8"

curl -s -b cookies.txt \
  "http://localhost:8080/v1/repos/${REPO_ID}/items/${FQN_B64}/callers?depth=3&limit=50" | jq .
```

---

### GET /v1/repos/{repo_id}/items/{fqn_b64}/callees

List all functions transitively called by the target item. Forward BFS traversal of `CALLS` and `CALL_INSTANTIATES` edges from the root. Same response schema, pagination, and provenance semantics as the [callers endpoint](#get-v1reposrepo_iditemsfqn_b64callers).

**Auth required**: verified session **or** API key with `read` scope

**Path parameters**: same as callers
**Query parameters**: same as callers
**Response 200**: same schema as callers (edges point forward instead of backward)
**Error responses**: same as callers

**Example**

```bash
curl -s -b cookies.txt \
  "http://localhost:8080/v1/repos/${REPO_ID}/items/${FQN_B64}/callees?depth=3&limit=50" | jq .
```

---

### GET /v1/repos/{repo_id}/items/{fqn_b64}/impls

List all impl blocks for a trait identified by `fqn_b64` within a repository. Returns both direct impls (`impl Trait for Type`) and blanket impls (`impl<T: Bound> Trait for T`) found in the graph.

**Auth required**: verified session **or** API key (any scope)

**Path parameters**:
- `repo_id` — UUID of the repository (must belong to the caller's tenant)
- `fqn_b64` — URL-safe base64 (no padding) encoded trait FQN

**Response 200**
```json
{
  "repo_id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
  "trait_fqn": "my_crate::MyTrait",
  "impls": [
    {
      "fqn": "my_crate::Foo",
      "impl_kind": "direct"
    },
    {
      "fqn": "my_crate::GenericFoo",
      "impl_kind": "blanket"
    }
  ]
}
```

| Field | Type | Description |
|-------|------|-------------|
| `repo_id` | UUID | Repository UUID |
| `trait_fqn` | string | FQN of the queried trait |
| `impls[].fqn` | string | FQN of the impl block |
| `impls[].impl_kind` | string | `"direct"` for concrete impls; `"blanket"` for blanket impls |

**Response 400** — `invalid_input` (malformed base64)
**Response 401** — `unauthorized` or `session_expired`
**Response 403** — `email_not_verified` or `insufficient_scope`
**Response 404** — repository not found or belongs to another tenant
**Response 503** — Neo4j graph not configured on this instance

**Example**

```bash
TRAIT_FQN="my_crate::MyTrait"
TRAIT_B64=$(printf '%s' "$TRAIT_FQN" | base64 -w0 | tr '+/' '-_' | tr -d '=')

curl -s -b cookies.txt \
  "http://localhost:8080/v1/repos/${REPO_ID}/items/${TRAIT_B64}/impls" | jq .
```

---

### GET /v1/repos/{repo_id}/items/{fqn_b64}/usages

List all usages of a type identified by `fqn_b64` within a repository. Returns two categories of usage combined in a flat list:

- **Textual** (`usage_kind = "textual"`) — items that reference the type by name (`USES_TYPE` edge)
- **Monomorphized** (`usage_kind = "monomorphized"`) — `TypeInstance` nodes derived from this type via a `MONOMORPHIZED_FROM` edge

**Auth required**: verified session **or** API key (any scope)

**Path parameters**:
- `repo_id` — UUID of the repository (must belong to the caller's tenant)
- `fqn_b64` — URL-safe base64 (no padding) encoded type FQN

**Response 200**
```json
{
  "repo_id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
  "type_fqn": "my_crate::MyType",
  "usages": [
    {
      "fqn": "my_crate::uses_it",
      "usage_kind": "textual"
    },
    {
      "fqn": "my_crate::MyType<i32>",
      "usage_kind": "monomorphized"
    }
  ]
}
```

| Field | Type | Description |
|-------|------|-------------|
| `repo_id` | UUID | Repository UUID |
| `type_fqn` | string | FQN of the queried type |
| `usages[].fqn` | string | FQN of the using item or type instance |
| `usages[].usage_kind` | string | `"textual"` or `"monomorphized"` |

**Response 400** — `invalid_input` (malformed base64)
**Response 401** — `unauthorized` or `session_expired`
**Response 403** — `email_not_verified` or `insufficient_scope`
**Response 404** — repository not found or belongs to another tenant
**Response 503** — Neo4j graph not configured on this instance

**Example**

```bash
TYPE_FQN="my_crate::MyType"
TYPE_B64=$(printf '%s' "$TYPE_FQN" | base64 -w0 | tr '+/' '-_' | tr -d '=')

curl -s -b cookies.txt \
  "http://localhost:8080/v1/repos/${REPO_ID}/items/${TYPE_B64}/usages" | jq .
```

---

## Graph query endpoint

### POST /v1/graph/query

Execute an arbitrary Cypher query against the tenant's Neo4j graph store. The tenant label is automatically injected into every node pattern so queries are isolated to the calling tenant's data.

**Security**: this endpoint exposes raw Cypher execution. It is restricted to admin-level access.

**Auth required**: API key with `admin` scope **or** session with `owner`/`admin` tenant role

**Request**
```json
{
  "cypher": "MATCH (n:Item) WHERE n.kind = $kind RETURN n.fqn LIMIT 10",
  "params": { "kind": "FN" },
  "read_only": true
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `cypher` | string | — | Raw Cypher statement. Tenant label injected automatically. Multi-statement queries (semicolons outside strings) are rejected. |
| `params` | object | `{}` | Named parameters bound into the query (`$key` → value) |
| `read_only` | bool | `true` | When `true`, pre-flight check blocks write operators (`CREATE`, `MERGE`, `SET`, `DELETE`, `DETACH`, `REMOVE`) |

**Response 200**
```json
{
  "rows": [
    { "n.fqn": "my_crate::auth::verify_token" },
    { "n.fqn": "my_crate::auth::extract_session" }
  ],
  "row_count": 2
}
```

| Field | Type | Description |
|-------|------|-------------|
| `rows` | array | Each element is a JSON object mapping column names to their values |
| `row_count` | int | Number of rows returned |

**Response 400** — `invalid_input` (multi-statement query) or `cypher_write_denied` (write operators detected in read-only mode)
**Response 401** — `unauthorized` or `session_expired`
**Response 403** — `insufficient_role` or `insufficient_scope`
**Response 503** — `graph_not_configured` (Neo4j not configured on this instance)

**Example — read-only query with API key**

```bash
curl -s -X POST http://localhost:8080/v1/graph/query \
  -H "Authorization: Bearer $ADMIN_KEY" \
  -H 'Content-Type: application/json' \
  -d '{
    "cypher": "MATCH (n:Item)-[:CALLS]->(m:Item) RETURN n.fqn, m.fqn LIMIT 5",
    "read_only": true
  }' | jq .
```

---

## Consistency health endpoint

### GET /v1/health/consistency

Kafka consistency metrics. Reports consumer lag and time since last event for each data-plane store. Admin-only because these metrics expose internal pipeline internals.

**Auth required**: API key with `admin` scope **or** session with `owner`/`admin` tenant role

**Response 200**
```json
{
  "checked_at": "2026-05-06T12:30:00Z",
  "stores": {
    "kafka": {
      "lag_messages": 42,
      "last_event_at": "2026-05-06T12:29:55Z",
      "status": "healthy"
    }
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `checked_at` | ISO 8601 | Timestamp of this check |
| `stores.kafka.lag_messages` | int | Number of unconsumed Kafka messages |
| `stores.kafka.last_event_at` | ISO 8601? | Timestamp of the most recent Kafka event; `null` if no event has ever been received |
| `stores.kafka.status` | string | `"healthy"` (< 30 s since last event), `"degraded"` (30–300 s), `"stale"` (> 300 s or never) |

**Response 401** — `unauthorized` or `session_expired`
**Response 403** — `insufficient_role` or `insufficient_scope`

**Example**

```bash
curl -s -H "Authorization: Bearer $ADMIN_KEY" \
  http://localhost:8080/v1/health/consistency | jq .
```

---

## Agent session endpoints (Wave 7)

The agent subsystem lets tenants run AI coding sessions backed by runtime adapters (Claude Code, OpenCode). Sessions are created via the REST API; events stream back over SSE. See [ADR-009](decisions/ADR-009-agent-execution-architecture.md) for the full design.

### POST /v1/agents/sessions

Create a new agent session. Publishes a start command to Kafka (`rb.agent.commands`); the `agent-runner` service picks it up and spawns the subprocess.

**Auth required**: verified session **or** API key with `write` scope

Rate-limited per tenant: 10 creates/min (`RB_SESSION_CREATE_RATE_LIMIT`), 100 concurrent sessions per tenant (`RB_TENANT_SESSION_CAP`).

### GET /v1/agents/sessions

List agent sessions for the current tenant. Supports filtering by status.

**Auth required**: verified session **or** API key with `read` scope

### GET /v1/agents/sessions/{id}

Get a single agent session by ID.

**Auth required**: verified session **or** API key with `read` scope

### DELETE /v1/agents/sessions/{id}

Terminate and delete an agent session. Publishes a terminate command to Kafka.

**Auth required**: verified session **or** API key with `write` scope

### GET /v1/agents/sessions/{id}/events

**SSE event stream.** Long-lived connection that pushes `RuntimeEvent` records as they arrive. Supports `?from_sequence=N` to resume from a known position.

**Auth required**: verified session **or** API key with `read` scope

**Content-Type**: `text/event-stream`

### GET /v1/agents/sessions/{id}/events/history

Replay all historical events for a session (non-streaming JSON array). Useful for hydrating the frontend viewer after a page reload.

**Auth required**: verified session **or** API key with `read` scope

### GET /v1/agents/sessions/{id}/log.ndjson

Download the full session log as newline-delimited JSON. Each line is a `RuntimeEvent`.

**Auth required**: verified session **or** API key with `read` scope

Refer to `openapi.json` for the full request/response schemas (session object, event types, error codes).

---

## MCP endpoint (Wave 7)

### POST /mcp

Streamable HTTP JSON-RPC 2.0 endpoint implementing the [Model Context Protocol](https://modelcontextprotocol.io/). Exposes the code intelligence surface as MCP tools so external AI agents can query the codebase programmatically.

**Auth required**: API key with `read` scope (passed via MCP session initialization)

**Content-Type**: `application/json` (JSON-RPC 2.0)

**Supported methods**:

| Method | Description |
|--------|-------------|
| `initialize` | Start an MCP session; returns server capabilities |
| `tools/list` | Enumerate available tools |
| `tools/call` | Invoke a tool by name |

**Available tools**:

| Tool | Description |
|------|-------------|
| `search_items` | Semantic search via Ollama + Qdrant |
| `get_item` | Item lookup by FQN |
| `get_callers` | Backward BFS call-graph traversal |
| `get_callees` | Forward BFS call-graph traversal |
| `get_trait_impls` | Trait implementation enumeration |
| `run_query` | Raw Cypher query (requires `admin` scope) |

See the `rb-mcp` crate and [ADR-009 §6.2](decisions/ADR-009-agent-execution-architecture.md) for tool schemas and security model.

---

## GitHub integration endpoints

### GET /v1/github/install-url

Generate the GitHub App installation URL for the current tenant. Redirects the user to GitHub to install the app on their organization/account.

**Auth required**: verified session

### GET /v1/github/callback

OAuth callback handler. GitHub redirects here after app installation. Binds the installation to the calling tenant. See [ADR-010](decisions/ADR-010-github-app-tenant-install.md) for the single-tenant lock invariant and orphan-reclaim flow.

**Auth required**: verified session (via session cookie in redirect)

### POST /v1/github/webhook

GitHub webhook receiver. Processes `installation`, `installation_repositories`, and `push` events. Verified via HMAC-SHA256 signature (`X-Hub-Signature-256`).

**Auth**: webhook secret (not user auth)

### GET /v1/github/installations/{id}/available-repos

List repositories accessible to a GitHub App installation. Used by the frontend to let the user select which repos to connect.

**Auth required**: verified session

### GET /v1/health/github-app

GitHub App health check. Returns the app identity and installation count.

**Auth**: public (unauthenticated)

**Operator runbooks**: [github-install-rebind](runbooks/github-install-rebind.md) covers cross-tenant installation collision diagnosis and resolution.
