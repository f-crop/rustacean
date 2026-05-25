# Auth and Migrations Sanity Check

End-to-end validation of the auth flow and migration state after a deploy on the
mars dev/UAT stack.

## Audience and when to run

Operators and agents validating a fresh deploy, a migration-touching PR, or a
stack rebuild. Run after any merge that touches `services/control-api/`,
`migrations/`, or auth-related crates. Also useful as a smoke test after a cold
start or database restore.

## Prerequisites

- mars dev stack running
- control-api healthy: `curl -s http://localhost:18080/health | jq .status`
  returns `"ok"`
- Direct database access: `docker compose exec postgres psql -U rustbrain`
- The `migrate` service has run (it runs automatically before `control-api`
  starts via compose `depends_on`)

```bash
export COMPOSE_CMD="docker compose --env-file compose/tailscale.env \
  -f compose/dev.yml -f compose/tailscale.yml"
```

## Steps

### 1. Check migration state

Verify the `migrate` service ran cleanly:

```bash
$COMPOSE_CMD logs migrate --tail=30
```

Look for: migration version numbers applied without errors. The last line should
indicate success (not a panic or connection failure).

Verify `control.schema_migrations` HEAD matches the latest migration file:

```bash
# Latest applied migration in the database
$COMPOSE_CMD exec postgres psql -U rustbrain -c \
  "SELECT version, description, applied_at
   FROM control.schema_migrations
   ORDER BY version DESC LIMIT 5;"

# Latest migration file on disk
ls -1 migrations/control/ | sort -n | tail -5
```

The highest `version` in the DB should match the highest-numbered file in
`migrations/control/`. If they differ, the migration service did not run or
failed mid-way.

### 2. Sign up a test user

```bash
TEST_EMAIL="sanity-$(date +%s)@test.local"

SIGNUP_RESPONSE=$(curl -s -D /tmp/sanity-headers.txt \
  -X POST http://localhost:18080/v1/auth/signup \
  -H "Content-Type: application/json" \
  -d "{
    \"email\": \"$TEST_EMAIL\",
    \"password\": \"sanity-check-12chars\",
    \"tenant_name\": \"Sanity Test $(date +%H%M%S)\"
  }")

echo "$SIGNUP_RESPONSE" | jq .

SESSION=$(grep -i set-cookie /tmp/sanity-headers.txt \
  | sed 's/.*rb_session=\([^;]*\).*/\1/')
echo "Session: $SESSION"
```

Expected: HTTP 201 with `email_verification_required: true` and a session
cookie.

### 3. Verify the email

Extract the verification token from control-api logs (console email transport on
mars):

```bash
$COMPOSE_CMD logs control-api --tail=100 \
  | grep -A5 "$TEST_EMAIL" | grep -i "token\|verify"
```

Submit the token:

```bash
curl -s -X POST http://localhost:18080/v1/auth/verify-email \
  -H "Content-Type: application/json" \
  -d "{\"token\": \"<token-from-logs>\"}"
```

Expected: HTTP 204 (no body).

### 4. Log in

```bash
LOGIN_RESPONSE=$(curl -s -D /tmp/sanity-login-headers.txt \
  -X POST http://localhost:18080/v1/auth/login \
  -H "Content-Type: application/json" \
  -d "{
    \"email\": \"$TEST_EMAIL\",
    \"password\": \"sanity-check-12chars\"
  }")

echo "$LOGIN_RESPONSE" | jq .

SESSION=$(grep -i set-cookie /tmp/sanity-login-headers.txt \
  | sed 's/.*rb_session=\([^;]*\).*/\1/')
```

Expected: HTTP 200 with `email_verification_required: false`.

### 5. Get current user

```bash
curl -s http://localhost:18080/v1/me \
  -H "Cookie: rb_session=$SESSION" | jq .
```

Expected: `user.email_verified` is `true`, `current_tenant.role` is `"owner"`.

### 6. List repos (GitHub integration check)

```bash
curl -s -o /dev/null -w "%{http_code}" \
  http://localhost:18080/v1/github/repos \
  -H "Cookie: rb_session=$SESSION"
```

Expected: HTTP 200 (empty list if no GitHub App installed) or HTTP 404 (no
installation for this tenant). Either is acceptable — the point is that the
endpoint responds without a 500.

### 7. Clean up test data

```bash
# Get tenant details
TENANT_INFO=$(curl -s http://localhost:18080/v1/me \
  -H "Cookie: rb_session=$SESSION" | jq -r '.current_tenant | "\(.id) \(.slug)"')
TENANT_ID=$(echo $TENANT_INFO | awk '{print $1}')
TENANT_SLUG=$(echo $TENANT_INFO | awk '{print $2}')

# Delete tenant
curl -s -X DELETE "http://localhost:18080/v1/tenants/$TENANT_ID" \
  -H "Cookie: rb_session=$SESSION" \
  -H "X-Confirm: $TENANT_SLUG" | jq .

# Delete user (direct SQL)
USER_ID=$(echo "$SIGNUP_RESPONSE" | jq -r .user_id)
$COMPOSE_CMD exec postgres psql -U rustbrain -c "
  BEGIN;
  DELETE FROM control.sessions WHERE user_id = '$USER_ID';
  DELETE FROM control.email_tokens WHERE user_id = '$USER_ID';
  DELETE FROM control.auth_events WHERE user_id = '$USER_ID';
  DELETE FROM control.tenant_members WHERE user_id = '$USER_ID';
  DELETE FROM control.users WHERE id = '$USER_ID';
  COMMIT;
"
```

## Verification

```bash
# 1. Migration version matches disk
DB_VERSION=$($COMPOSE_CMD exec postgres psql -U rustbrain -tAc \
  "SELECT MAX(version) FROM control.schema_migrations;")
DISK_VERSION=$(ls migrations/control/ | sed 's/_.*//' | sort -n | tail -1)
[ "$DB_VERSION" = "$DISK_VERSION" ] && echo "MIGRATION PASS" || \
  echo "MIGRATION FAIL: db=$DB_VERSION disk=$DISK_VERSION"

# 2. Full auth flow completes (steps 2-5 succeed without 4xx/5xx)

# 3. No stuck migrations
$COMPOSE_CMD exec postgres psql -U rustbrain -c \
  "SELECT pid, state, query, query_start
   FROM pg_stat_activity
   WHERE datname = 'rustbrain'
     AND state = 'idle in transaction'
     AND query_start < now() - interval '5 minutes';"
```

Pass criterion: migration versions match, full auth flow completes, no stuck
transactions.

## Failure modes

### Stuck migration (locked row in pg)

Symptoms: `migrate` service hangs on startup, `pg_stat_activity` shows an `idle
in transaction` entry with a `schema_migrations` query.

```bash
# Find the stuck backend
$COMPOSE_CMD exec postgres psql -U rustbrain -c \
  "SELECT pid, state, query, query_start
   FROM pg_stat_activity
   WHERE datname = 'rustbrain' AND state = 'idle in transaction';"

# Terminate the stuck backend (use the PID from above)
$COMPOSE_CMD exec postgres psql -U rustbrain -c \
  "SELECT pg_terminate_backend(<pid>);"

# Re-run migrations
$COMPOSE_CMD restart migrate
$COMPOSE_CMD logs migrate --tail=20
```

### Email verification token replay

Symptoms: `POST /v1/auth/verify-email` returns 400 with `invalid_token`.

The token may be expired (1-hour TTL) or already used (`used_at IS NOT NULL`).
Check:

```bash
$COMPOSE_CMD exec postgres psql -U rustbrain -c \
  "SELECT kind, expires_at, used_at, created_at
   FROM control.email_tokens
   WHERE user_id = '$USER_ID'
   ORDER BY created_at DESC LIMIT 5;"
```

If all tokens are expired or used, resend:

```bash
curl -s -X POST http://localhost:18080/v1/auth/resend-verification \
  -H "Cookie: rb_session=$SESSION"
```

### Session cookie domain misconfiguration

Symptoms: login succeeds but `GET /v1/me` returns 401 — the browser is not
sending the `rb_session` cookie.

On mars, `RB_SECURE_COOKIES=false` must be set in the compose environment
(HTTP, not HTTPS). The cookie is set with `HttpOnly; SameSite=Lax; Path=/`
and no explicit `Domain` attribute.

Check the current setting:

```bash
$COMPOSE_CMD exec control-api env | grep RB_SECURE_COOKIES
```

If missing or `true` while running over HTTP, cookies will be rejected by the
browser. Fix by adding `RB_SECURE_COOKIES=false` to `compose/tailscale.env` and
restarting control-api.

### Migration checksum mismatch

Symptoms: `migrate` service panics with `checksum mismatch for migration NNN`.

A previously applied migration file was modified on disk. The migration runner
stores SHA-256 checksums and rejects changes. **Never** edit applied migration
files. To fix:

1. Restore the original migration file from git:
   ```bash
   git checkout main -- migrations/control/<NNN>_<name>.sql
   ```
2. Write the schema change as a new migration with the next version number.
3. Re-run migrations:
   ```bash
   $COMPOSE_CMD restart migrate
   ```

## Verification record

| Field | Value |
|-------|-------|
| Exercised on | mars (100.87.157.74) |
| Date | 2026-05-25T11:43:00Z |
| Git SHA | `b9b1ca47` (`main` HEAD at exercise time) |
| Operator | Technical Writer agent |
