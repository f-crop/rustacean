/// Integration tests for the PG migration runner.
///
/// Requires a running Postgres instance. Set `TEST_DATABASE_URL` to run them.
/// The compose/test.yml stack (port 5433) provides the test database:
///   docker compose -f compose/test.yml up -d postgres
///   `TEST_DATABASE_URL=postgres://postgres:postgres@localhost:5433/postgres cargo test -p migrate`
use std::path::Path;

use migrate::{migrate_control, migrate_tenant};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tempfile::TempDir;

// ── helpers ──────────────────────────────────────────────────────────────────

fn test_pool_url() -> Option<String> {
    std::env::var("TEST_DATABASE_URL").ok()
}

async fn connect(url: &str) -> PgPool {
    PgPoolOptions::new()
        .max_connections(2)
        .connect(url)
        .await
        .expect("failed to connect to test DB")
}

fn make_repo(subdir: &str, files: &[(&str, &str)]) -> TempDir {
    let root = TempDir::new().unwrap();
    let dir = root.path().join("migrations").join(subdir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, sql) in files {
        std::fs::write(dir.join(name), sql).unwrap();
    }
    root
}

fn add_migration(root: &Path, subdir: &str, filename: &str, sql: &str) {
    let path = root.join("migrations").join(subdir).join(filename);
    std::fs::write(path, sql).unwrap();
}

async fn drop_schema(pool: &PgPool, schema: &str) {
    sqlx::query(&format!(r#"DROP SCHEMA IF EXISTS "{schema}" CASCADE"#))
        .execute(pool)
        .await
        .unwrap();
}

fn random_tenant_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!("{ns:024x}")
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_control_applies_then_idempotent() {
    let Some(url) = test_pool_url() else {
        eprintln!("SKIP test_control_applies_then_idempotent: TEST_DATABASE_URL not set");
        return;
    };

    let pool = connect(&url).await;
    drop_schema(&pool, "control").await;

    let repo = make_repo("control", &[("001_baseline.sql", "SELECT 1;"), ("002_second.sql", "SELECT 2;")]);

    let n1 = migrate_control(&pool, repo.path()).await.expect("first run failed");
    assert_eq!(n1, 2, "expected 2 applied on first run");

    let n2 = migrate_control(&pool, repo.path()).await.expect("second run failed");
    assert_eq!(n2, 0, "expected 0 applied on second run (idempotent)");

    drop_schema(&pool, "control").await;
}

#[tokio::test]
async fn test_control_checksum_mismatch_rejected() {
    let Some(url) = test_pool_url() else {
        eprintln!("SKIP test_control_checksum_mismatch_rejected: TEST_DATABASE_URL not set");
        return;
    };

    let pool = connect(&url).await;
    drop_schema(&pool, "control").await;

    let repo = make_repo("control", &[("001_baseline.sql", "SELECT 1;")]);
    migrate_control(&pool, repo.path()).await.expect("first run failed");

    // Tamper with the applied migration
    add_migration(repo.path(), "control", "001_baseline.sql", "SELECT 99; -- tampered");

    let err = migrate_control(&pool, repo.path()).await.unwrap_err();
    assert!(err.to_string().contains("checksum mismatch"), "unexpected error: {err}");

    drop_schema(&pool, "control").await;
}

#[tokio::test]
async fn test_tenant_creates_schema_and_applies() {
    let Some(url) = test_pool_url() else {
        eprintln!("SKIP test_tenant_creates_schema_and_applies: TEST_DATABASE_URL not set");
        return;
    };

    let pool = connect(&url).await;
    let tid = random_tenant_id();
    let schema = format!("tenant_{tid}");
    drop_schema(&pool, &schema).await;

    let repo = make_repo("tenant", &[("001_initial.sql", "SELECT 1;")]);

    let n = migrate_tenant(&pool, &tid, repo.path()).await.expect("tenant migration failed");
    assert_eq!(n, 1);

    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_namespace WHERE nspname = $1)",
    )
    .bind(&schema)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(exists, "tenant schema must exist after migration");

    drop_schema(&pool, &schema).await;
}

#[tokio::test]
async fn test_multi_statement_migration_applied() {
    let Some(url) = test_pool_url() else {
        eprintln!("SKIP test_multi_statement_migration_applied: TEST_DATABASE_URL not set");
        return;
    };

    let pool = connect(&url).await;
    drop_schema(&pool, "control").await;

    // Two CREATE TABLE statements in a single migration file — the bug case.
    let multi_sql =
        "CREATE TABLE foo_ms (id INT PRIMARY KEY);\nCREATE TABLE bar_ms (id INT PRIMARY KEY);";
    let repo = make_repo("control", &[("001_multi.sql", multi_sql)]);

    let n = migrate_control(&pool, repo.path())
        .await
        .expect("multi-statement migration failed");
    assert_eq!(n, 1, "one migration file applied");

    for table in &["foo_ms", "bar_ms"] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM information_schema.tables \
             WHERE table_schema = 'control' AND table_name = $1)",
        )
        .bind(*table)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(exists, "table '{table}' must exist after multi-statement migration");
    }

    drop_schema(&pool, "control").await;
}

#[tokio::test]
async fn test_failed_migration_rolled_back() {
    let Some(url) = test_pool_url() else {
        eprintln!("SKIP test_failed_migration_rolled_back: TEST_DATABASE_URL not set");
        return;
    };

    let pool = connect(&url).await;
    drop_schema(&pool, "control").await;

    let repo = make_repo("control", &[("001_good.sql", "SELECT 1;")]);
    migrate_control(&pool, repo.path()).await.expect("good migration failed");

    // Add an invalid migration
    add_migration(repo.path(), "control", "002_bad.sql", "NOT VALID SQL !!!");
    let err = migrate_control(&pool, repo.path()).await.unwrap_err();
    assert!(err.to_string().contains("database"), "expected DB error, got: {err}");

    // v002 must not be recorded — transaction rolled back
    let max: Option<i32> =
        sqlx::query_scalar(r#"SELECT MAX(version) FROM "control".schema_migrations"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(max, Some(1), "schema must remain at v001 after failed v002");

    drop_schema(&pool, "control").await;
}
