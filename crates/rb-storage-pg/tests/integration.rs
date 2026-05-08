/// Integration tests for `TenantPool`.
///
/// Requires a running Postgres instance. Set `TEST_DATABASE_URL` to run:
///   docker compose -f compose/test.yml up -d postgres
///   `TEST_DATABASE_URL=postgres://rustbrain:rustbrain@localhost:5433/rustbrain` \
///     cargo test -p rb-storage-pg
use rb_schemas::TenantId;
use rb_storage_pg::TenantPool;
use rb_tenant::TenantCtx;
use sqlx::postgres::PgPoolOptions;

// ── helpers ──────────────────────────────────────────────────────────────────

fn test_url() -> Option<String> {
    std::env::var("TEST_DATABASE_URL").ok()
}

async fn make_pool(url: &str) -> TenantPool {
    let pg = PgPoolOptions::new()
        .max_connections(3)
        .connect(url)
        .await
        .expect("connect to test DB");
    TenantPool::new(pg)
}

fn new_ctx() -> TenantCtx {
    TenantCtx::new(TenantId::new())
}

macro_rules! skip_no_db {
    ($url:ident) => {
        let Some($url) = test_url() else {
            eprintln!("SKIP: TEST_DATABASE_URL not set");
            return;
        };
    };
}

// ── schema lifecycle ──────────────────────────────────────────────────────────

#[tokio::test]
async fn schema_does_not_exist_before_create() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx = new_ctx();
    let exists = pool.schema_exists(&ctx).await.unwrap();
    assert!(!exists, "fresh tenant schema must not pre-exist");
}

#[tokio::test]
async fn create_schema_creates_the_schema() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx = new_ctx();
    pool.create_schema(&ctx).await.unwrap();
    let exists = pool.schema_exists(&ctx).await.unwrap();
    assert!(exists, "schema must exist after create_schema");
    pool.drop_schema(&ctx).await.unwrap();
}

#[tokio::test]
async fn create_schema_is_idempotent() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx = new_ctx();
    pool.create_schema(&ctx).await.unwrap();
    // Second call must not error (IF NOT EXISTS).
    pool.create_schema(&ctx).await.unwrap();
    pool.drop_schema(&ctx).await.unwrap();
}

#[tokio::test]
async fn drop_schema_removes_the_schema() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx = new_ctx();
    pool.create_schema(&ctx).await.unwrap();
    pool.drop_schema(&ctx).await.unwrap();
    let exists = pool.schema_exists(&ctx).await.unwrap();
    assert!(!exists, "schema must be gone after drop_schema");
}

#[tokio::test]
async fn drop_nonexistent_schema_is_ok() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx = new_ctx();
    // Must not error on a schema that was never created.
    pool.drop_schema(&ctx).await.unwrap();
}

#[tokio::test]
async fn schema_lifecycle_create_drop_create() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx = new_ctx();
    pool.create_schema(&ctx).await.unwrap();
    pool.drop_schema(&ctx).await.unwrap();
    // Re-creating after drop must succeed.
    pool.create_schema(&ctx).await.unwrap();
    assert!(pool.schema_exists(&ctx).await.unwrap());
    pool.drop_schema(&ctx).await.unwrap();
}

// ── multi-tenant coexistence ──────────────────────────────────────────────────

#[tokio::test]
async fn multiple_tenants_coexist() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let (ctx_a, ctx_b, ctx_c) = (new_ctx(), new_ctx(), new_ctx());
    pool.create_schema(&ctx_a).await.unwrap();
    pool.create_schema(&ctx_b).await.unwrap();
    pool.create_schema(&ctx_c).await.unwrap();
    assert!(pool.schema_exists(&ctx_a).await.unwrap());
    assert!(pool.schema_exists(&ctx_b).await.unwrap());
    assert!(pool.schema_exists(&ctx_c).await.unwrap());
    pool.drop_schema(&ctx_a).await.unwrap();
    pool.drop_schema(&ctx_b).await.unwrap();
    pool.drop_schema(&ctx_c).await.unwrap();
}

#[tokio::test]
async fn different_tenant_ids_get_different_schemas() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx_a = new_ctx();
    let ctx_b = new_ctx();
    assert_ne!(
        ctx_a.schema_name(),
        ctx_b.schema_name(),
        "distinct UUIDs must produce distinct schema names"
    );
    pool.create_schema(&ctx_a).await.unwrap();
    pool.create_schema(&ctx_b).await.unwrap();
    pool.drop_schema(&ctx_a).await.unwrap();
    pool.drop_schema(&ctx_b).await.unwrap();
}

#[tokio::test]
async fn control_pool_can_execute_queries() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let n: i64 = sqlx::query_scalar("SELECT 1::bigint")
        .fetch_one(pool.control())
        .await
        .unwrap();
    assert_eq!(n, 1);
}

// ── cross-tenant isolation ────────────────────────────────────────────────────

/// Helper: create a test table inside a tenant schema and insert a row.
async fn create_test_table(pool: &TenantPool, ctx: &TenantCtx) {
    let tbl = ctx.qualify("_iso_test");
    sqlx::query(&format!("CREATE TABLE IF NOT EXISTS {tbl} (val TEXT)"))
        .execute(pool.control())
        .await
        .unwrap();
    sqlx::query(&format!("INSERT INTO {tbl} (val) VALUES ('hello')"))
        .execute(pool.control())
        .await
        .unwrap();
}

#[tokio::test]
async fn isolation_table_in_a_not_visible_in_b() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx_a = new_ctx();
    let ctx_b = new_ctx();
    pool.create_schema(&ctx_a).await.unwrap();
    pool.create_schema(&ctx_b).await.unwrap();

    create_test_table(&pool, &ctx_a).await;

    // Table must not exist in schema B.
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(\
           SELECT 1 FROM information_schema.tables \
           WHERE table_schema = $1 AND table_name = '_iso_test'\
         )",
    )
    .bind(ctx_b.schema_name())
    .fetch_one(pool.control())
    .await
    .unwrap();
    assert!(!exists, "_iso_test must not exist in tenant B's schema");

    pool.drop_schema(&ctx_a).await.unwrap();
    pool.drop_schema(&ctx_b).await.unwrap();
}

#[tokio::test]
async fn isolation_insert_in_a_not_in_b() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx_a = new_ctx();
    let ctx_b = new_ctx();
    pool.create_schema(&ctx_a).await.unwrap();
    pool.create_schema(&ctx_b).await.unwrap();

    create_test_table(&pool, &ctx_a).await;
    // Create same table in B but insert no rows.
    let tbl_b = ctx_b.qualify("_iso_test");
    sqlx::query(&format!("CREATE TABLE IF NOT EXISTS {tbl_b} (val TEXT)"))
        .execute(pool.control())
        .await
        .unwrap();

    let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {tbl_b}"))
        .fetch_one(pool.control())
        .await
        .unwrap();
    assert_eq!(count, 0, "tenant B must have no rows from tenant A");

    pool.drop_schema(&ctx_a).await.unwrap();
    pool.drop_schema(&ctx_b).await.unwrap();
}

#[tokio::test]
async fn isolation_drop_a_leaves_b_intact() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx_a = new_ctx();
    let ctx_b = new_ctx();
    pool.create_schema(&ctx_a).await.unwrap();
    pool.create_schema(&ctx_b).await.unwrap();
    create_test_table(&pool, &ctx_a).await;
    create_test_table(&pool, &ctx_b).await;

    pool.drop_schema(&ctx_a).await.unwrap();

    assert!(!pool.schema_exists(&ctx_a).await.unwrap(), "A must be gone");
    assert!(
        pool.schema_exists(&ctx_b).await.unwrap(),
        "B must survive A's drop"
    );

    pool.drop_schema(&ctx_b).await.unwrap();
}

#[tokio::test]
async fn isolation_qualify_routes_to_correct_schema() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx_a = new_ctx();
    let ctx_b = new_ctx();
    pool.create_schema(&ctx_a).await.unwrap();
    pool.create_schema(&ctx_b).await.unwrap();

    create_test_table(&pool, &ctx_a).await;
    create_test_table(&pool, &ctx_b).await;

    let count_a: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {}",
        ctx_a.qualify("_iso_test")
    ))
    .fetch_one(pool.control())
    .await
    .unwrap();
    let count_b: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {}",
        ctx_b.qualify("_iso_test")
    ))
    .fetch_one(pool.control())
    .await
    .unwrap();
    assert_eq!(count_a, 1, "qualify must route to A");
    assert_eq!(count_b, 1, "qualify must route to B, not A");

    pool.drop_schema(&ctx_a).await.unwrap();
    pool.drop_schema(&ctx_b).await.unwrap();
}

#[tokio::test]
async fn isolation_control_queries_unaffected_by_tenant_schemas() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx = new_ctx();
    pool.create_schema(&ctx).await.unwrap();

    // control() pool can still query information_schema regardless of tenant schemas.
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.schemata")
        .fetch_one(pool.control())
        .await
        .unwrap();
    assert!(
        n > 0,
        "control pool must be unaffected by tenant schema operations"
    );

    pool.drop_schema(&ctx).await.unwrap();
}

// ── schema name validation ────────────────────────────────────────────────────

#[tokio::test]
async fn tenant_schema_name_matches_pg_pattern() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx = new_ctx();
    pool.create_schema(&ctx).await.unwrap();

    // The pg_namespace query used by migrate_all_tenants must find this schema.
    let found: bool = sqlx::query_scalar(
        "SELECT EXISTS(\
           SELECT 1 FROM pg_catalog.pg_namespace \
           WHERE nspname ~ '^tenant_[0-9a-f]{24}$' \
             AND nspname = $1\
         )",
    )
    .bind(ctx.schema_name())
    .fetch_one(pool.control())
    .await
    .unwrap();
    assert!(
        found,
        "schema name must match the ^tenant_[0-9a-f]{{24}}$ pg pattern"
    );

    pool.drop_schema(&ctx).await.unwrap();
}

#[tokio::test]
async fn schema_name_deterministic_across_pool_instances() {
    skip_no_db!(url);
    let pool1 = make_pool(&url).await;
    let pool2 = make_pool(&url).await;
    let tid = TenantId::new();
    let ctx1 = TenantCtx::new(tid);
    let ctx2 = TenantCtx::new(tid);
    assert_eq!(
        ctx1.schema_name(),
        ctx2.schema_name(),
        "same TenantId must produce same schema name regardless of pool"
    );
    pool1.create_schema(&ctx1).await.unwrap();
    assert!(pool2.schema_exists(&ctx2).await.unwrap());
    pool1.drop_schema(&ctx1).await.unwrap();
}
