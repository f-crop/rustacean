use std::path::Path;

use sqlx::PgPool;

use crate::error::MigrateError;
use crate::pg::runner::{MigrationStatus, Runner, migrations_dir};

const CONTROL_SCHEMA: &str = "control";

pub async fn migrate_control(pool: &PgPool, repo_root: &Path) -> Result<usize, MigrateError> {
    let dir = migrations_dir(repo_root, "control");
    let mut conn = pool.acquire().await?;
    let runner = Runner::new(CONTROL_SCHEMA, &dir);
    runner.bootstrap(&mut conn).await?;
    runner.apply_all(&mut conn).await
}

pub async fn control_status(
    pool: &PgPool,
    repo_root: &Path,
) -> Result<Vec<MigrationStatus>, MigrateError> {
    let dir = migrations_dir(repo_root, "control");
    let mut conn = pool.acquire().await?;
    let runner = Runner::new(CONTROL_SCHEMA, &dir);
    // Bootstrap ensures the tracking table exists before querying it.
    runner.bootstrap(&mut conn).await?;
    runner.status(&mut conn).await
}
