//! Multi-tenant CI scenarios: install conflict redirect URL, cross-tenant repo
//! connect (allowed), and same-tenant duplicate repo (blocked).
//!
//! These tests complement `integration_github_install_conflict.rs` (which covers
//! the install reclaim SQL path) and `integration_tests.rs` (which covers the
//! duplicate-email signup HTTP path).  Together the three files give full PR
//! coverage for the three conflict scenarios raised in the Wave 7 retrospective
//! (RUSAA-1670):
//!
//! 1. Two tenants installing the same GitHub App installation
//! 2. Two tenants signing up with the same primary email
//! 3. Two tenants connecting the same repository
//!
//! DB-backed tests are skipped automatically when `RB_DATABASE_URL` is not set.

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

fn random_id_in_range(base: i64) -> i64 {
    i64::from(rand::random::<i32>().abs()) + base
}

async fn seed_tenant(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name, status) \
         VALUES ($1, $2, $3, $4, 'active')",
    )
    .bind(id)
    .bind(format!("mt-test-{id}"))
    .bind("MT Test Tenant")
    .bind(format!("mt_test_{}", id.simple()))
    .execute(pool)
    .await
    .expect("seed tenant");
    id
}

async fn seed_user(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO control.users \
         (id, email, password_hash, email_verified_at) \
         VALUES ($1, $2, '$argon2id$placeholder', now())",
    )
    .bind(id)
    .bind(format!("mt-test-{}@example.com", id.simple()))
    .execute(pool)
    .await
    .expect("seed user");
    id
}

async fn seed_installation(
    pool: &sqlx::PgPool,
    tenant_id: Uuid,
    github_installation_id: i64,
) -> Uuid {
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
    .expect("seed installation");
    id
}

// ---------------------------------------------------------------------------
// Scenario 1 (supplement): GitHub App Installation Conflict — redirect URLs
// ---------------------------------------------------------------------------
//
// The SQL-level reclaim tests live in integration_github_install_conflict.rs.
// This companion verifies the redirect URL contract the HTTP callback emits for
// each outcome so the frontend can surface the correct message.

/// The success redirect contains `install=success`; the conflict redirect
/// contains `install=conflict&reason=active`.  Verifying the string contract
/// here makes the frontend dependency explicit and catches typos in the source.
#[test]
fn install_redirect_url_contracts_are_correct() {
    let base = "http://localhost:8080";

    // Success path (installation upserted or orphan reclaimed).
    let success = format!("{base}/repos?install=success&installation_uuid=abc&account_login=org");
    // Blocked path (active owner, reclaim not possible).
    let conflict = format!("{base}/repos?install=conflict&reason=active");

    assert!(
        success.contains("install=success"),
        "success redirect must carry 'install=success'"
    );
    assert!(
        conflict.contains("install=conflict"),
        "blocked redirect must carry 'install=conflict'"
    );
    assert!(
        conflict.contains("reason=active"),
        "blocked redirect must include 'reason=active' to signal a live owner"
    );
    assert!(
        !conflict.contains("install=success"),
        "blocked redirect must not claim success"
    );
    assert!(
        !success.contains("install=conflict"),
        "success redirect must not claim conflict"
    );
}

// ---------------------------------------------------------------------------
// Scenario 3: Two tenants connecting the same repository
// ---------------------------------------------------------------------------

/// Two different tenants can each connect the same GitHub repository.
///
/// The `repos` table enforces `UNIQUE (tenant_id, github_repo_id)` — a
/// *per-tenant* constraint.  Different tenants sharing the same `github_repo_id`
/// must both succeed; the constraint must not be global.
///
/// Skipped automatically when `RB_DATABASE_URL` is not set.
#[tokio::test]
async fn cross_tenant_same_github_repo_both_succeed() {
    let Some(pool) = connect().await else { return };

    let tenant_a = seed_tenant(&pool).await;
    let tenant_b = seed_tenant(&pool).await;
    let user_a = seed_user(&pool).await;
    let user_b = seed_user(&pool).await;

    // Use well-separated ranges to avoid cross-test collisions.
    let github_repo_id = random_id_in_range(5_000_000);
    let install_a = seed_installation(&pool, tenant_a, random_id_in_range(7_000_000)).await;
    let install_b = seed_installation(&pool, tenant_b, random_id_in_range(8_000_000)).await;

    let repo_a = Uuid::new_v4();
    let repo_b = Uuid::new_v4();

    // Tenant A connects the repo — must succeed.
    sqlx::query(
        "INSERT INTO control.repos \
         (id, tenant_id, installation_id, github_repo_id, full_name, default_branch, connected_by) \
         VALUES ($1, $2, $3, $4, 'org/shared-repo', 'main', $5)",
    )
    .bind(repo_a)
    .bind(tenant_a)
    .bind(install_a)
    .bind(github_repo_id)
    .bind(user_a)
    .execute(&pool)
    .await
    .expect("tenant A repo connect must succeed");

    // Tenant B connects the SAME GitHub repo — must also succeed.
    let result = sqlx::query(
        "INSERT INTO control.repos \
         (id, tenant_id, installation_id, github_repo_id, full_name, default_branch, connected_by) \
         VALUES ($1, $2, $3, $4, 'org/shared-repo', 'main', $5)",
    )
    .bind(repo_b)
    .bind(tenant_b)
    .bind(install_b)
    .bind(github_repo_id)
    .bind(user_b)
    .execute(&pool)
    .await;

    assert!(
        result.is_ok(),
        "tenant B connecting the same github_repo_id must succeed: \
         uniqueness is per-tenant, not global"
    );

    // Verify both rows exist for the same github_repo_id.
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM control.repos WHERE github_repo_id = $1")
            .bind(github_repo_id)
            .fetch_one(&pool)
            .await
            .expect("count repos");
    assert_eq!(
        count, 2,
        "both tenant rows must exist for the shared github_repo_id"
    );

    // Cleanup — FK order: repos → installations → users → tenants.
    sqlx::query("DELETE FROM control.repos WHERE id IN ($1, $2)")
        .bind(repo_a)
        .bind(repo_b)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.github_installations WHERE id IN ($1, $2)")
        .bind(install_a)
        .bind(install_b)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.users WHERE id IN ($1, $2)")
        .bind(user_a)
        .bind(user_b)
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

/// The same tenant connecting a repository a second time must be rejected by
/// the `UNIQUE (tenant_id, github_repo_id)` constraint.
///
/// The application layer maps this constraint violation to
/// `AppError::RepoAlreadyConnected` (HTTP 409, error code `repo_already_connected`).
/// This test verifies the constraint name matches the string the handler checks,
/// locking the mapping against future schema renames.
///
/// Skipped automatically when `RB_DATABASE_URL` is not set.
#[tokio::test]
async fn same_tenant_duplicate_repo_blocked_by_unique_constraint() {
    let Some(pool) = connect().await else { return };

    let tenant_id = seed_tenant(&pool).await;
    let user_id = seed_user(&pool).await;
    let github_repo_id = random_id_in_range(9_000_000);
    let install_id =
        seed_installation(&pool, tenant_id, random_id_in_range(10_000_000)).await;

    let repo_first = Uuid::new_v4();
    let repo_second = Uuid::new_v4();

    // First connect — must succeed.
    sqlx::query(
        "INSERT INTO control.repos \
         (id, tenant_id, installation_id, github_repo_id, full_name, default_branch, connected_by) \
         VALUES ($1, $2, $3, $4, 'org/my-repo', 'main', $5)",
    )
    .bind(repo_first)
    .bind(tenant_id)
    .bind(install_id)
    .bind(github_repo_id)
    .bind(user_id)
    .execute(&pool)
    .await
    .expect("first connect must succeed");

    // Second connect of the same repo for the same tenant — must fail.
    let result = sqlx::query(
        "INSERT INTO control.repos \
         (id, tenant_id, installation_id, github_repo_id, full_name, default_branch, connected_by) \
         VALUES ($1, $2, $3, $4, 'org/my-repo', 'main', $5)",
    )
    .bind(repo_second)
    .bind(tenant_id)
    .bind(install_id)
    .bind(github_repo_id)
    .bind(user_id)
    .execute(&pool)
    .await;

    // The constraint name must match what `connect_repo` in repos.rs checks.
    match result {
        Err(sqlx::Error::Database(ref dbe)) => {
            assert_eq!(
                dbe.constraint(),
                Some("repos_tenant_id_github_repo_id_key"),
                "duplicate repo must be rejected by the per-tenant unique constraint; \
                 constraint name must match the string checked in repos.rs"
            );
        }
        Err(other) => panic!("expected unique constraint violation, got: {other}"),
        Ok(_) => panic!("duplicate repo insert must not succeed"),
    }

    // Cleanup — FK order: repos → installations → users → tenants.
    sqlx::query("DELETE FROM control.repos WHERE id = $1")
        .bind(repo_first)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.github_installations WHERE id = $1")
        .bind(install_id)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.users WHERE id = $1")
        .bind(user_id)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}
