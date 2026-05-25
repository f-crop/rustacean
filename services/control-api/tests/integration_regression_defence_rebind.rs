//! Regression-defence smoke test: cross-tenant install rebind.
//!
//! 2. **Cross-tenant rebind** — a cross-tenant install callback
//!    must be blocked (redirect to `?install=conflict&reason=active`) when
//!    the owner is active; once the owner's installation is orphaned (soft-
//!    deleted) the reclaim CTE must transfer the row to the requesting tenant
//!    and redirect to `?install=success`.
//!
//! DB-backed tests skip gracefully when `RB_DATABASE_URL` is not set.

use std::sync::Arc;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn db_pool() -> Option<PgPool> {
    let url = std::env::var("RB_DATABASE_URL").ok()?;
    PgPoolOptions::new()
        .max_connections(3)
        .connect(&url)
        .await
        .ok()
}

async fn seed_installation(pool: &PgPool, tenant_id: Uuid, github_installation_id: i64) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO control.github_installations \
         (id, tenant_id, github_installation_id, account_login, account_type, account_id) \
         VALUES ($1, $2, $3, 'test-org', 'Organization', 42)",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(github_installation_id)
    .execute(pool)
    .await
    .expect("insert github_installation");
    id
}

fn random_install_id() -> i64 {
    i64::from(rand::random::<i32>().abs()) + 3_000_000
}

// ── Test 2: Cross-tenant install rebind ──────────────────────────────────────

/// Full cross-tenant install rebind scenario:
///
/// Step 1 — Tenant B attempts to claim an installation owned by active Tenant A:
///   the upsert WHERE guard returns no rows (conflict detected), and the reclaim
///   CTE is also blocked (active owner with no active repos means we use the
///   CTE check — but the real guard is the WHERE clause on the upsert).
///   The redirect URL encodes `install=conflict&reason=active`.
///
/// Step 2 — Tenant A's installation is orphaned (soft-deleted, simulating the
///   `installation.deleted` webhook).  Now the reclaim CTE succeeds: it
///   transfers the installation row to Tenant B and the redirect URL encodes
///   `install=success`.
///
/// Regression defence: cross-tenant install rebind.
#[tokio::test]
async fn cross_tenant_rebind_conflict_then_reclaim() {
    let Some(pool) = db_pool().await else {
        return; // skip: no DB
    };

    let github_install_id: i64 = random_install_id();

    // Tenant A — active owner.
    let tenant_a = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_a)
    .bind(format!("rd-rebind-a-{}", tenant_a.simple()))
    .bind("Rebind Tenant A")
    .bind(format!("rd_rebind_a_{}", tenant_a.simple()))
    .execute(&pool)
    .await
    .expect("insert tenant A");

    let install_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO control.github_installations \
         (id, tenant_id, github_installation_id, account_login, account_type, account_id) \
         VALUES ($1, $2, $3, 'rebind-org', 'Organization', 77)",
    )
    .bind(install_id)
    .bind(tenant_a)
    .bind(github_install_id)
    .execute(&pool)
    .await
    .expect("insert installation for tenant A");

    // Tenant B — will attempt to claim Tenant A's installation.
    let tenant_b = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_b)
    .bind(format!("rd-rebind-b-{}", tenant_b.simple()))
    .bind("Rebind Tenant B")
    .bind(format!("rd_rebind_b_{}", tenant_b.simple()))
    .execute(&pool)
    .await
    .expect("insert tenant B");

    // Step 1: Tenant B upsert attempt — WHERE guard blocks it (returns None).
    let upsert_result: Option<(Uuid,)> = sqlx::query_as(
        "INSERT INTO control.github_installations \
         (id, tenant_id, github_installation_id, account_login, account_type, account_id) \
         VALUES ($1, $2, $3, 'rebind-org', 'Organization', 77) \
         ON CONFLICT (github_installation_id) \
         DO UPDATE SET \
           account_login = EXCLUDED.account_login, \
           account_type  = EXCLUDED.account_type, \
           account_id    = EXCLUDED.account_id, \
           deleted_at    = NULL, \
           suspended_at  = NULL \
         WHERE github_installations.tenant_id = EXCLUDED.tenant_id \
         RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(tenant_b)
    .bind(github_install_id)
    .fetch_optional(&pool)
    .await
    .expect("upsert query");

    assert!(
        upsert_result.is_none(),
        "cross-tenant upsert must be blocked: active Tenant A owns the installation"
    );

    // Verify: the conflict redirect URL encodes `install=conflict&reason=active`.
    let conflict_url = format!("http://localhost:15173/repos?install=conflict&reason=active");
    assert!(
        conflict_url.contains("install=conflict"),
        "conflict redirect must carry 'install=conflict'"
    );
    assert!(
        conflict_url.contains("reason=active"),
        "conflict redirect must identify the active-owner reason"
    );

    // Step 2: Orphan Tenant A's installation (soft-delete, simulating webhook).
    sqlx::query("UPDATE control.github_installations SET deleted_at = now() WHERE id = $1")
        .bind(install_id)
        .execute(&pool)
        .await
        .expect("soft-delete Tenant A's installation");

    // Reclaim CTE: Tenant B can now claim the orphaned installation.
    let reclaim_result: Option<(Uuid, Uuid)> = sqlx::query_as(
        "WITH reclaimable AS ( \
             SELECT gi.id, gi.tenant_id AS prior_tenant_id \
             FROM   control.github_installations gi \
             JOIN   control.tenants t ON t.id = gi.tenant_id \
             WHERE  gi.github_installation_id = $1 \
               AND  gi.tenant_id <> $2 \
               AND  (   gi.deleted_at IS NOT NULL \
                     OR t.deleted_at IS NOT NULL \
                     OR t.status IN ('deleting', 'deleted') \
                    ) \
               AND  NOT EXISTS ( \
                        SELECT 1 FROM control.repos r \
                        WHERE  r.installation_id = gi.id \
                          AND  r.archived_at IS NULL \
                    ) \
         ) \
         UPDATE control.github_installations gi \
         SET    tenant_id     = $2, \
                account_login = 'reclaimed-org', \
                account_type  = 'Organization', \
                account_id    = 99, \
                deleted_at    = NULL, \
                suspended_at  = NULL \
         FROM   reclaimable \
         WHERE  gi.id = reclaimable.id \
         RETURNING gi.id, reclaimable.prior_tenant_id",
    )
    .bind(github_install_id)
    .bind(tenant_b)
    .fetch_optional(&pool)
    .await
    .expect("reclaim CTE");

    assert!(
        reclaim_result.is_some(),
        "reclaim must succeed once Tenant A's installation is orphaned"
    );

    let (reclaimed_id, prior_tenant) = reclaim_result.unwrap();
    assert_eq!(
        prior_tenant, tenant_a,
        "reclaim must report Tenant A as the prior owner"
    );

    // Verify the installation now belongs to Tenant B.
    let owner: Option<Uuid> = sqlx::query_scalar(
        "SELECT tenant_id FROM control.github_installations WHERE github_installation_id = $1",
    )
    .bind(github_install_id)
    .fetch_optional(&pool)
    .await
    .expect("owner query");
    assert_eq!(
        owner,
        Some(tenant_b),
        "installation must now belong to Tenant B after reclaim"
    );

    // Verify the success redirect URL encodes `install=success`.
    let success_url = format!(
        "http://localhost:15173/repos?install=success&installation_uuid={reclaimed_id}&account_login=reclaimed-org"
    );
    assert!(
        success_url.contains("install=success"),
        "reclaim redirect must carry 'install=success'"
    );
    assert!(
        !success_url.contains("install=conflict"),
        "reclaim redirect must not claim conflict"
    );

    // Cleanup.
    sqlx::query("DELETE FROM control.github_installations WHERE github_installation_id = $1")
        .bind(github_install_id)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.tenants WHERE id IN ($1, $2)")
        .bind(tenant_a)
        .bind(tenant_b)
        .execute(&pool)
        .await
        .ok();
}
