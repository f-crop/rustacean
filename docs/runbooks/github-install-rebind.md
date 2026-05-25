# GitHub Install Rebind

Diagnose and resolve cross-tenant GitHub App installation conflicts on the mars
dev/UAT stack.

For the design context behind the single-tenant lock and the self-heal
mechanism, see
[ADR-010](../decisions/ADR-010-github-app-tenant-install.md).

## Audience and when to run

Operators who see a `/repos?install=conflict` redirect after installing the
GitHub App on a tenant whose GitHub account was previously connected to a
different (now-abandoned) Rustacean tenant. In most cases the server-side
self-heal (PR #574, `8edd18f6`) resolves this automatically — this runbook
covers how to confirm the self-heal fired and the manual fallback for genuine
cross-account conflicts.

## Prerequisites

- mars dev stack running
- control-api healthy: `curl -s http://localhost:18080/health | jq .status`
  returns `"ok"`
- Direct database access: `docker compose exec postgres psql -U rustbrain`
- The GitHub App is installed on the target GitHub account/org

## Happy path: self-heal (nothing to do)

Since PR #574 (`8edd18f6`), the install callback attempts an atomic
orphan-reclaim before showing the conflict page. The reclaim succeeds when:

1. The existing installation row is soft-deleted (`deleted_at IS NOT NULL`),
   **or** its owning tenant is terminal (`tenants.deleted_at IS NOT NULL` or
   `tenants.status IN ('deleting', 'deleted')`); **and**
2. No active repos are linked (`repos.archived_at IS NULL` check).

If both conditions hold, the row's `tenant_id` is transferred to the new tenant
in a single SQL statement and the user is redirected to
`/repos?install=success`.

### Confirm the self-heal fired

Check the control-api logs for the reclaim trace:

```bash
docker compose --env-file compose/tailscale.env \
  -f compose/dev.yml -f compose/tailscale.yml \
  logs control-api --tail=200 | grep "orphaned installation reclaimed"
```

A successful reclaim logs:

```
github callback: orphaned installation reclaimed
  requesting_tenant=<new-tenant-uuid>
  prior_tenant=<old-tenant-uuid>
  installation_id=<github-install-id>
  installation_uuid=<internal-uuid>
  account=<github-login>
```

Verify the DB state:

```bash
docker compose --env-file compose/tailscale.env \
  -f compose/dev.yml -f compose/tailscale.yml \
  exec postgres psql -U rustbrain -c \
  "SELECT id, tenant_id, github_installation_id, account_login, deleted_at, suspended_at
   FROM control.github_installations
   ORDER BY created_at DESC LIMIT 5;"
```

The reclaimed row should show `tenant_id` = the new tenant, `deleted_at = NULL`,
`suspended_at = NULL`.

## Fallback: manual SQL rebind (genuine cross-account conflict)

When the conflict is `reason=active` — meaning the existing installation belongs
to a live, non-deleted tenant — the self-heal does not fire. The control-api
logs show:

```
github callback: cross-tenant installation conflict rejected (active owner)
  requesting_tenant=<new-tenant-uuid>
  installation_id=<github-install-id>
```

This is a genuine conflict: two live tenants are trying to use the same GitHub
App installation. Only human judgement can decide which tenant should own it.

### Steps

#### 1. Identify the conflicting installation

```bash
# Find the installation row by GitHub installation ID
GITHUB_INSTALL_ID=12345  # from the GitHub webhook or URL params

docker compose --env-file compose/tailscale.env \
  -f compose/dev.yml -f compose/tailscale.yml \
  exec postgres psql -U rustbrain -c \
  "SELECT gi.id, gi.tenant_id, gi.github_installation_id,
          gi.account_login, gi.account_type,
          t.name AS tenant_name, t.status AS tenant_status, t.deleted_at
   FROM control.github_installations gi
   JOIN control.tenants t ON t.id = gi.tenant_id
   WHERE gi.github_installation_id = $GITHUB_INSTALL_ID;"
```

#### 2. Safety checks before rebind

```bash
# Confirm no active repos are linked to this installation
docker compose --env-file compose/tailscale.env \
  -f compose/dev.yml -f compose/tailscale.yml \
  exec postgres psql -U rustbrain -c \
  "SELECT r.id, r.full_name, r.archived_at
   FROM control.repos r
   JOIN control.github_installations gi ON gi.id = r.installation_id
   WHERE gi.github_installation_id = $GITHUB_INSTALL_ID
     AND r.archived_at IS NULL;"
```

If active repos exist, you must archive or disconnect them first. Rebinding an
installation with active repos will orphan ingestion runs and break repo access
for the old tenant.

#### 3. Execute the rebind

```bash
NEW_TENANT_ID="<target-tenant-uuid>"

docker compose --env-file compose/tailscale.env \
  -f compose/dev.yml -f compose/tailscale.yml \
  exec postgres psql -U rustbrain -c \
  "UPDATE control.github_installations
   SET tenant_id = '$NEW_TENANT_ID',
       deleted_at = NULL,
       suspended_at = NULL
   WHERE github_installation_id = $GITHUB_INSTALL_ID;"
```

Expected: `UPDATE 1`. If `UPDATE 0`, the `github_installation_id` does not
exist — double-check the value.

#### 4. Verify the rebind

```bash
# Internal UUID of the installation row
INSTALL_UUID="<installation-uuid-from-step-1>"

curl -s "http://localhost:18080/v1/github/repos" \
  -H "Cookie: rb_session=$SESSION" | jq .
```

Expected: HTTP 200 with the list of available repos for the installation.
A 409 means the rebind did not take effect — re-check the installation ID and
tenant ID.

## Verification

```bash
# 1. Installation row points to the correct tenant
docker compose --env-file compose/tailscale.env \
  -f compose/dev.yml -f compose/tailscale.yml \
  exec postgres psql -U rustbrain -c \
  "SELECT id, tenant_id, github_installation_id, account_login,
          deleted_at, suspended_at
   FROM control.github_installations
   WHERE github_installation_id = $GITHUB_INSTALL_ID;"

# 2. Repos endpoint returns 200 (not 409)
curl -s -o /dev/null -w "%{http_code}" \
  "http://localhost:18080/v1/github/repos" \
  -H "Cookie: rb_session=$SESSION"
```

Pass criterion: installation `tenant_id` matches the target tenant, `deleted_at`
and `suspended_at` are NULL, and `/v1/github/repos` returns HTTP 200.

## Rollback

To reverse a manual rebind, re-run the same `UPDATE` with the original
`tenant_id`:

```bash
ORIGINAL_TENANT_ID="<prior-tenant-uuid>"

docker compose --env-file compose/tailscale.env \
  -f compose/dev.yml -f compose/tailscale.yml \
  exec postgres psql -U rustbrain -c \
  "UPDATE control.github_installations
   SET tenant_id = '$ORIGINAL_TENANT_ID'
   WHERE github_installation_id = $GITHUB_INSTALL_ID;"
```

## Verification record

| Field | Value |
|-------|-------|
| Exercised on | mars (100.87.157.74) |
| Date | 2026-05-25T11:43:00Z |
| Git SHA | `b9b1ca47` (`main` HEAD at exercise time) |
| Operator | Technical Writer agent |
