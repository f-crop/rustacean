//! Integration tests for the orphan-reclaim path in `GET /v1/github/callback`.
//!
//! These tests exercise the SQL reclaim CTE directly, mirroring the pattern
//! used by `upsert_guard_rejects_cross_tenant_owner` in `install.rs`.
//!
//! Skipped automatically when `RB_DATABASE_URL` is not set.

use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn connect() -> Option<sqlx::PgPool> {
    let Ok(db_url) = std::env::var("RB_DATABASE_URL") else {
        return None;
    };
    Some(
        PgPoolOptions::new()
            .max_connections(2)
            .connect(&db_url)
            .await
            .expect("connect to test database"),
    )
}

fn random_install_id() -> i64 {
    i64::from(rand::random::<i32>().abs()) + 2_000_000
}

async fn seed_tenant(pool: &sqlx::PgPool, id: Uuid, status: &str) {
    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name, status) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(format!("reclaim-test-{id}"))
    .bind("Reclaim Test Tenant")
    .bind(format!("reclaim_{}", id.simple()))
    .bind(status)
    .execute(pool)
    .await
    .expect("seed tenant");
}

async fn seed_installation(
    pool: &sqlx::PgPool,
    id: Uuid,
    tenant_id: Uuid,
    github_installation_id: i64,
    deleted: bool,
) {
    let sql = if deleted {
        "INSERT INTO control.github_installations \
         (id, tenant_id, github_installation_id, account_login, account_type, account_id, \
          deleted_at) \
         VALUES ($1, $2, $3, 'test-org', 'Organization', 42, now())"
    } else {
        "INSERT INTO control.github_installations \
         (id, tenant_id, github_installation_id, account_login, account_type, account_id) \
         VALUES ($1, $2, $3, 'test-org', 'Organization', 42)"
    };
    sqlx::query(sql)
        .bind(id)
        .bind(tenant_id)
        .bind(github_installation_id)
        .execute(pool)
        .await
        .expect("seed github_installation");
}

async fn cleanup(pool: &sqlx::PgPool, installation_id: i64, tenant_ids: &[Uuid]) {
    sqlx::query("DELETE FROM control.github_installations WHERE github_installation_id = $1")
        .bind(installation_id)
        .execute(pool)
        .await
        .ok();
    for tid in tenant_ids {
        sqlx::query("DELETE FROM control.tenants WHERE id = $1")
            .bind(tid)
            .execute(pool)
            .await
            .ok();
    }
}

// ---------------------------------------------------------------------------
// Reclaim CTE (mirrors the production SQL in install.rs)
// ---------------------------------------------------------------------------

async fn try_reclaim(
    pool: &sqlx::PgPool,
    github_installation_id: i64,
    requesting_tenant: Uuid,
) -> Option<(Uuid, Uuid)> {
    sqlx::query_as(
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
                        SELECT 1 \
                        FROM   control.repos r \
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
    .bind(github_installation_id)
    .bind(requesting_tenant)
    .fetch_optional(pool)
    .await
    .expect("reclaim CTE query")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Reclaim succeeds when the prior owner's installation row has `deleted_at` set
/// (GitHub sent an `installation.deleted` webhook that soft-deleted the row).
#[tokio::test]
async fn reclaim_succeeds_when_installation_soft_deleted() {
    let Some(pool) = connect().await else { return };

    let prior_tenant = Uuid::new_v4();
    let new_tenant = Uuid::new_v4();
    let install_row = Uuid::new_v4();
    let github_id = random_install_id();

    seed_tenant(&pool, prior_tenant, "active").await;
    seed_tenant(&pool, new_tenant, "active").await;
    seed_installation(
        &pool,
        install_row,
        prior_tenant,
        github_id,
        /* deleted */ true,
    )
    .await;

    let result = try_reclaim(&pool, github_id, new_tenant).await;

    assert!(
        result.is_some(),
        "reclaim must succeed when installation is soft-deleted"
    );
    let (returned_id, prior_id) = result.unwrap();
    assert_eq!(
        returned_id, install_row,
        "reclaim returns the installation UUID"
    );
    assert_eq!(
        prior_id, prior_tenant,
        "reclaim returns the prior tenant UUID"
    );

    // Verify the row now belongs to the new tenant.
    let owner: Option<Uuid> = sqlx::query_scalar(
        "SELECT tenant_id FROM control.github_installations WHERE github_installation_id = $1",
    )
    .bind(github_id)
    .fetch_optional(&pool)
    .await
    .expect("owner query");
    assert_eq!(
        owner,
        Some(new_tenant),
        "row must now belong to the new tenant"
    );

    cleanup(&pool, github_id, &[prior_tenant, new_tenant]).await;
}

/// Reclaim succeeds when the owning tenant is in 'deleted' status (tenant was
/// torn down but the installation row was never soft-deleted via webhook).
#[tokio::test]
async fn reclaim_succeeds_when_owner_tenant_deleted() {
    let Some(pool) = connect().await else { return };

    let prior_tenant = Uuid::new_v4();
    let new_tenant = Uuid::new_v4();
    let install_row = Uuid::new_v4();
    let github_id = random_install_id();

    seed_tenant(&pool, prior_tenant, "deleted").await;
    seed_tenant(&pool, new_tenant, "active").await;
    seed_installation(
        &pool,
        install_row,
        prior_tenant,
        github_id,
        /* deleted */ false,
    )
    .await;

    let result = try_reclaim(&pool, github_id, new_tenant).await;

    assert!(
        result.is_some(),
        "reclaim must succeed when owner tenant is in 'deleted' status"
    );

    cleanup(&pool, github_id, &[prior_tenant, new_tenant]).await;
}

/// Reclaim is blocked when the owner's installation has at least one active repo
/// (archived_at IS NULL). The active owner must not lose their installation.
#[tokio::test]
async fn reclaim_blocked_when_owner_has_active_repos() {
    let Some(pool) = connect().await else { return };

    let prior_tenant = Uuid::new_v4();
    let new_tenant = Uuid::new_v4();
    let install_row = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let github_id = random_install_id();

    // prior_tenant has the installation soft-deleted BUT has an active repo —
    // the repo check must override and block the reclaim.
    seed_tenant(&pool, prior_tenant, "active").await;
    seed_tenant(&pool, new_tenant, "active").await;

    // Seed a user for the repo FK.
    let user_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, email_verified_at) \
         VALUES ($1, $2, '$argon2id$placeholder', now())",
    )
    .bind(user_id)
    .bind(format!("reclaim-test-{}@example.com", user_id.simple()))
    .execute(&pool)
    .await
    .expect("seed user");

    seed_installation(
        &pool,
        install_row,
        prior_tenant,
        github_id,
        /* deleted */ true,
    )
    .await;

    // Active repo linked to the installation (archived_at IS NULL).
    sqlx::query(
        "INSERT INTO control.repos \
         (id, tenant_id, installation_id, github_repo_id, full_name, default_branch, \
          connected_by) \
         VALUES ($1, $2, $3, $4, 'org/repo', 'main', $5)",
    )
    .bind(repo_id)
    .bind(prior_tenant)
    .bind(install_row)
    .bind(github_id)
    .bind(user_id)
    .execute(&pool)
    .await
    .expect("seed active repo");

    let result = try_reclaim(&pool, github_id, new_tenant).await;

    assert!(
        result.is_none(),
        "reclaim must be blocked when active repos are linked to the installation"
    );

    // Cleanup: repos first (FK), then installations, then tenants + user.
    sqlx::query("DELETE FROM control.repos WHERE id = $1")
        .bind(repo_id)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.users WHERE id = $1")
        .bind(user_id)
        .execute(&pool)
        .await
        .ok();
    cleanup(&pool, github_id, &[prior_tenant, new_tenant]).await;
}
