# Tenant Provisioning

Provision a new tenant on the mars dev/UAT stack: signup, email verification,
first tenant creation, and optional platform-admin promotion.

## Audience and when to run

Operators bootstrapping a fresh Rustacean tenant for development, QA, or demo
purposes on mars. Run this after a clean stack deploy or whenever a new isolated
workspace is needed.

## Prerequisites

- mars dev stack running (`docker compose` via `compose/dev.yml` +
  `compose/tailscale.yml`)
- control-api healthy: `curl -s http://localhost:18080/health | jq .status`
  returns `"ok"`
- Direct database access: `psql` via `docker compose exec postgres psql -U
  rustbrain`
- Email transport: on mars the default is `console` — verification tokens appear
  in `control-api` container logs instead of real email. If
  `RB_AUTO_VERIFY_EMAIL=true` is set (common in dev), signup returns
  `email_verification_required: false` and step 2 can be skipped

## Steps

### 1. Sign up a new user

```bash
curl -s -X POST http://localhost:18080/v1/auth/signup \
  -H "Content-Type: application/json" \
  -d '{
    "email": "operator@example.com",
    "password": "change-me-strong-12chars",
    "tenant_name": "UAT Workspace"
  }' | jq .
```

Expected response (HTTP 201):

```json
{
  "email_verification_required": true,
  "user_id": "<uuid>"
}
```

The response also sets a `Set-Cookie: rb_session=<token>` header. Save it:

```bash
SESSION=$(curl -s -D- -o /dev/null -X POST http://localhost:18080/v1/auth/signup \
  -H "Content-Type: application/json" \
  -d '{
    "email": "operator@example.com",
    "password": "change-me-strong-12chars",
    "tenant_name": "UAT Workspace"
  }' | grep -i set-cookie | sed 's/.*rb_session=\([^;]*\).*/\1/')

echo "Session token: $SESSION"
```

Signup atomically creates the user, a tenant, a `tenant_<uuid>` PostgreSQL
schema, the `owner` membership row, a verification email token, and an initial
session.

### 2. Verify the email address

On mars with console email transport, the verification token is in the
control-api logs:

```bash
docker compose --env-file compose/tailscale.env \
  -f compose/dev.yml -f compose/tailscale.yml \
  logs control-api --tail=50 | grep -i "verification"
```

Look for a line containing the plaintext token. Then verify:

```bash
curl -s -X POST http://localhost:18080/v1/auth/verify-email \
  -H "Content-Type: application/json" \
  -d '{
    "token": "<token-from-logs>"
  }'
```

Expected: HTTP 204 (no body). The user's `email_verified_at` is now set.

If the token expired (1-hour TTL), resend:

```bash
curl -s -X POST http://localhost:18080/v1/auth/resend-verification \
  -H "Cookie: rb_session=$SESSION"
```

### 3. Log in (if session expired)

```bash
curl -s -X POST http://localhost:18080/v1/auth/login \
  -H "Content-Type: application/json" \
  -d '{
    "email": "operator@example.com",
    "password": "change-me-strong-12chars"
  }' | jq .
```

Expected response (HTTP 200):

```json
{
  "user_id": "<uuid>",
  "tenant_id": "<uuid>",
  "email_verification_required": false
}
```

### 4. Confirm identity and tenant

```bash
curl -s http://localhost:18080/v1/me \
  -H "Cookie: rb_session=$SESSION" | jq .
```

Expected: `user.email_verified` is `true`, `current_tenant.role` is `"owner"`,
and `current_tenant.name` matches the name from signup.

### 5. Promote to platform admin (optional)

Platform-admin is a deployment-wide flag (not per-tenant) that gates admin
endpoints such as GitHub App manifest creation. There is no API endpoint —
promotion is direct SQL only.

```bash
# Get the user UUID from step 1 or from GET /v1/me
USER_ID="<user-uuid>"

docker compose --env-file compose/tailscale.env \
  -f compose/dev.yml -f compose/tailscale.yml \
  exec postgres psql -U rustbrain -c \
  "UPDATE control.users SET is_platform_admin = true WHERE id = '$USER_ID';"
```

Expected: `UPDATE 1`.

Verify the promotion took effect — the session picks it up on next request:

```bash
curl -s http://localhost:18080/v1/me \
  -H "Cookie: rb_session=$SESSION" | jq .user
```

## Verification

Run these checks after completing all steps:

```bash
# 1. GET /v1/me returns expected identity and role
curl -s http://localhost:18080/v1/me \
  -H "Cookie: rb_session=$SESSION" | jq '{
    email: .user.email,
    verified: .user.email_verified,
    role: .current_tenant.role,
    tenant: .current_tenant.name
  }'

# 2. Tenant row exists in control.tenants
docker compose --env-file compose/tailscale.env \
  -f compose/dev.yml -f compose/tailscale.yml \
  exec postgres psql -U rustbrain -c \
  "SELECT id, slug, name, status FROM control.tenants ORDER BY created_at DESC LIMIT 5;"

# 3. User linked via control.tenant_members
docker compose --env-file compose/tailscale.env \
  -f compose/dev.yml -f compose/tailscale.yml \
  exec postgres psql -U rustbrain -c \
  "SELECT tm.role, u.email, t.name
   FROM control.tenant_members tm
   JOIN control.users u ON u.id = tm.user_id
   JOIN control.tenants t ON t.id = tm.tenant_id
   ORDER BY tm.joined_at DESC LIMIT 5;"

# 4. Platform admin flag (if promoted)
docker compose --env-file compose/tailscale.env \
  -f compose/dev.yml -f compose/tailscale.yml \
  exec postgres psql -U rustbrain -c \
  "SELECT id, email, is_platform_admin FROM control.users WHERE email = 'operator@example.com';"
```

Pass criterion: all queries return the expected rows, `GET /v1/me` shows
`email_verified: true` and `role: "owner"`.

## Rollback

### Delete the tenant

Tenant deletion is soft-delete via the API. The owner must confirm with the
tenant slug:

```bash
# Get the tenant slug from GET /v1/me → current_tenant.slug
TENANT_ID="<tenant-uuid>"
TENANT_SLUG="<tenant-slug>"

curl -s -X DELETE "http://localhost:18080/v1/tenants/$TENANT_ID" \
  -H "Cookie: rb_session=$SESSION" \
  -H "X-Confirm: $TENANT_SLUG" | jq .
```

Expected: HTTP 202 with `"status": "deleting"`. The `tombstoner` service
asynchronously drops the tenant's PostgreSQL schema, removes Neo4j nodes, and
deletes Qdrant points.

Cascade behaviour:
- `control.tenants.status` → `'deleting'`, `deleted_at` set
- `control.tenant_members` rows remain (soft reference)
- `control.sessions` for this tenant are not automatically revoked (user can
  still log in to other tenants)
- The `tenant_<uuid>` PostgreSQL schema is dropped asynchronously by
  `tombstoner`

### Remove user record (manual, requires DB access)

There is no user soft-delete API. To fully clean up after a test:

```bash
docker compose --env-file compose/tailscale.env \
  -f compose/dev.yml -f compose/tailscale.yml \
  exec postgres psql -U rustbrain -c "
    BEGIN;
    DELETE FROM control.sessions WHERE user_id = '$USER_ID';
    DELETE FROM control.email_tokens WHERE user_id = '$USER_ID';
    DELETE FROM control.auth_events WHERE user_id = '$USER_ID';
    DELETE FROM control.tenant_members WHERE user_id = '$USER_ID';
    DELETE FROM control.users WHERE id = '$USER_ID';
    COMMIT;
  "
```

Run the tenant delete API first (Step above) so the tenant schema cleanup is
handled by `tombstoner`. Only delete the user row after confirming the tenant
deletion has completed (`control.tenants.status = 'deleted'`).

## Verification record

| Field | Value |
|-------|-------|
| Exercised on | mars (100.87.157.74) |
| Date | 2026-05-25T11:43:00Z |
| Git SHA | `b9b1ca47` (`main` HEAD at exercise time) |
| Operator | Technical Writer agent |
