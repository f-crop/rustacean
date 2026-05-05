# API Reference

**Base URL**: `http://localhost:8080` (local) · `http://100.87.157.74:18080` (mars/Tailscale)

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

---

## Health endpoints

### GET /health

Liveness probe. Always returns 200 while the process is running.

**Response 200**
```json
{ "status": "ok" }
```

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
