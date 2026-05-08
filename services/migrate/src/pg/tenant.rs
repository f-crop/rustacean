use std::path::{Path, PathBuf};

use sqlx::PgPool;

use crate::error::MigrateError;
use crate::pg::runner::{MigrationStatus, Runner, migrations_dir};

/// Apply tenant migrations for a tenant identified by hex ID.
///
/// The `pool` reference is consumed only to clone the pool — the returned
/// future is `'static` and captures no caller lifetimes.
pub fn migrate_tenant(
    pool: &PgPool,
    tenant_id: &str,
    repo_root: &Path,
) -> impl std::future::Future<Output = Result<usize, MigrateError>> + use<> {
    let pool = pool.clone();
    let schema = tenant_schema_name(tenant_id);
    let dir = migrations_dir(repo_root, "tenant");
    async move { migrate_tenant_schema_inner(pool, schema, dir).await }
}

/// Apply tenant migrations to an already-named schema from an explicit directory.
///
/// Used by `control-api` on signup: the caller already has the schema name and
/// the path to the `migrations/tenant/` directory, so no repo-root indirection
/// is needed.
///
/// The `pool` reference is consumed only to clone the pool — the returned
/// future is `'static` and captures no caller lifetimes.
// Used by `control-api` as a library consumer; not called from this binary.
#[allow(dead_code)]
pub fn migrate_tenant_schema(
    pool: &PgPool,
    schema: &str,
    tenant_migrations_dir: &Path,
) -> impl std::future::Future<Output = Result<usize, MigrateError>> + use<> {
    let pool = pool.clone();
    let schema = schema.to_owned();
    let dir = tenant_migrations_dir.to_owned();
    async move { migrate_tenant_schema_inner(pool, schema, dir).await }
}

async fn migrate_tenant_schema_inner(
    pool: PgPool,
    schema: String,
    dir: PathBuf,
) -> Result<usize, MigrateError> {
    let mut conn = pool.acquire().await?;
    let runner = Runner::new(&schema, &dir);
    runner.bootstrap(&mut conn).await?;
    runner.apply_all(&mut conn).await
}

/// Applies tenant migrations to all existing tenant schemas in parallel-safe order.
///
/// Each schema is migrated serially with a PG session advisory lock so that
/// two concurrent runners cannot migrate the same tenant at the same time.
/// If the lock is held by another runner the tenant is skipped.
pub async fn migrate_all_tenants(pool: &PgPool, repo_root: &Path) -> Result<usize, MigrateError> {
    let dir = migrations_dir(repo_root, "tenant");
    let schemas = tenant_schemas(pool).await?;
    let mut total = 0usize;

    for schema in &schemas {
        match migrate_tenant_locked(pool, schema, &dir).await {
            Ok(n) => total += n,
            Err(MigrateError::LockUnavailable(_)) => {
                tracing::info!(schema, "skipping — advisory lock held by another runner");
            }
            Err(e) => return Err(e),
        }
    }

    Ok(total)
}

pub async fn tenant_status(
    pool: &PgPool,
    tenant_id: &str,
    repo_root: &Path,
) -> Result<Vec<MigrationStatus>, MigrateError> {
    let schema = tenant_schema_name(tenant_id);
    let dir = migrations_dir(repo_root, "tenant");
    let mut conn = pool.acquire().await?;
    let runner = Runner::new(&schema, &dir);
    runner.bootstrap(&mut conn).await?;
    runner.status(&mut conn).await
}

pub async fn tenant_schemas(pool: &PgPool) -> Result<Vec<String>, MigrateError> {
    let schemas: Vec<String> = sqlx::query_scalar(
        "SELECT nspname FROM pg_catalog.pg_namespace \
         WHERE nspname ~ '^tenant_[0-9a-f]{24}$' \
         ORDER BY nspname",
    )
    .fetch_all(pool)
    .await?;
    Ok(schemas)
}

pub fn tenant_schema_name(tenant_id: &str) -> String {
    format!("tenant_{tenant_id}")
}

async fn migrate_tenant_locked(
    pool: &PgPool,
    schema: &str,
    dir: &Path,
) -> Result<usize, MigrateError> {
    // Acquire a session-level advisory lock on this schema.
    // pg_try_advisory_lock returns false immediately if another session holds it.
    let mut conn = pool.acquire().await?;

    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtext($1)::bigint)")
        .bind(format!("rb.migrate.{schema}"))
        .fetch_one(&mut *conn)
        .await?;

    if !locked {
        return Err(MigrateError::LockUnavailable(schema.to_string()));
    }

    let runner = Runner::new(schema, dir);
    runner.bootstrap(&mut conn).await?;
    let count = runner.apply_all(&mut conn).await?;

    sqlx::query("SELECT pg_advisory_unlock(hashtext($1)::bigint)")
        .bind(format!("rb.migrate.{schema}"))
        .execute(&mut *conn)
        .await?;

    Ok(count)
}
