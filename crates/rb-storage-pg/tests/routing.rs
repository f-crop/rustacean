/// Query routing integration tests for `TenantPool` (REQ-TN-03).
///
/// Proves that `TenantCtx::qualify()` routes every SQL statement to the correct
/// tenant schema with zero cross-tenant data leakage.
///
/// Requires a running Postgres instance. Set `TEST_DATABASE_URL` to run:
///   docker compose -f compose/test.yml up -d postgres
///   `TEST_DATABASE_URL=postgres://rustbrain:rustbrain@localhost:5433/rustbrain` \
///     cargo test -p rb-storage-pg --test routing
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

async fn create_routing_table(pool: &TenantPool, ctx: &TenantCtx) {
    let tbl = ctx.qualify("_routing_test");
    sqlx::query(&format!(
        "CREATE TABLE IF NOT EXISTS {tbl} (id SERIAL PRIMARY KEY, val INTEGER NOT NULL)"
    ))
    .execute(pool.control())
    .await
    .unwrap();
}

// ── query routing ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn routing_qualify_returns_schema_dot_table() {
    skip_no_db!(url);
    let _ = make_pool(&url).await;
    let ctx = new_ctx();
    let qualified = ctx.qualify("repos");
    assert!(
        qualified.starts_with("tenant_"),
        "must start with tenant_ prefix"
    );
    assert!(qualified.contains('.'), "must contain a dot separator");
    let parts: Vec<&str> = qualified.splitn(2, '.').collect();
    assert_eq!(
        parts[0],
        ctx.schema_name(),
        "left of dot must be schema name"
    );
    assert_eq!(parts[1], "repos", "right of dot must be table name");
}

#[tokio::test]
async fn routing_insert_count_in_a_is_zero_in_b() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx_a = new_ctx();
    let ctx_b = new_ctx();
    pool.create_schema(&ctx_a).await.unwrap();
    pool.create_schema(&ctx_b).await.unwrap();
    create_routing_table(&pool, &ctx_a).await;
    create_routing_table(&pool, &ctx_b).await;

    let tbl_a = ctx_a.qualify("_routing_test");
    sqlx::query(&format!("INSERT INTO {tbl_a} (val) VALUES (1),(2),(3)"))
        .execute(pool.control())
        .await
        .unwrap();

    let count_b: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {}",
        ctx_b.qualify("_routing_test")
    ))
    .fetch_one(pool.control())
    .await
    .unwrap();
    assert_eq!(
        count_b, 0,
        "tenant B must see zero rows inserted into tenant A"
    );

    pool.drop_schema(&ctx_a).await.unwrap();
    pool.drop_schema(&ctx_b).await.unwrap();
}

#[tokio::test]
async fn routing_update_affects_only_origin_tenant() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx_a = new_ctx();
    let ctx_b = new_ctx();
    pool.create_schema(&ctx_a).await.unwrap();
    pool.create_schema(&ctx_b).await.unwrap();
    create_routing_table(&pool, &ctx_a).await;
    create_routing_table(&pool, &ctx_b).await;

    let tbl_a = ctx_a.qualify("_routing_test");
    let tbl_b = ctx_b.qualify("_routing_test");
    sqlx::query(&format!("INSERT INTO {tbl_a} (val) VALUES (10)"))
        .execute(pool.control())
        .await
        .unwrap();
    sqlx::query(&format!("INSERT INTO {tbl_b} (val) VALUES (20)"))
        .execute(pool.control())
        .await
        .unwrap();
    sqlx::query(&format!("UPDATE {tbl_a} SET val = 99"))
        .execute(pool.control())
        .await
        .unwrap();

    let val_b: i32 = sqlx::query_scalar(&format!("SELECT val FROM {tbl_b}"))
        .fetch_one(pool.control())
        .await
        .unwrap();
    assert_eq!(
        val_b, 20,
        "UPDATE in tenant A must not affect tenant B's row"
    );

    pool.drop_schema(&ctx_a).await.unwrap();
    pool.drop_schema(&ctx_b).await.unwrap();
}

#[tokio::test]
async fn routing_delete_in_a_does_not_affect_b() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx_a = new_ctx();
    let ctx_b = new_ctx();
    pool.create_schema(&ctx_a).await.unwrap();
    pool.create_schema(&ctx_b).await.unwrap();
    create_routing_table(&pool, &ctx_a).await;
    create_routing_table(&pool, &ctx_b).await;

    let tbl_a = ctx_a.qualify("_routing_test");
    let tbl_b = ctx_b.qualify("_routing_test");
    sqlx::query(&format!("INSERT INTO {tbl_a} (val) VALUES (1)"))
        .execute(pool.control())
        .await
        .unwrap();
    sqlx::query(&format!("INSERT INTO {tbl_b} (val) VALUES (2)"))
        .execute(pool.control())
        .await
        .unwrap();
    sqlx::query(&format!("DELETE FROM {tbl_a}"))
        .execute(pool.control())
        .await
        .unwrap();

    let count_b: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {tbl_b}"))
        .fetch_one(pool.control())
        .await
        .unwrap();
    assert_eq!(
        count_b, 1,
        "DELETE in tenant A must not remove tenant B's rows"
    );

    pool.drop_schema(&ctx_a).await.unwrap();
    pool.drop_schema(&ctx_b).await.unwrap();
}

#[tokio::test]
async fn routing_sequential_mixed_operations_stay_isolated() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx_a = new_ctx();
    let ctx_b = new_ctx();
    pool.create_schema(&ctx_a).await.unwrap();
    pool.create_schema(&ctx_b).await.unwrap();
    create_routing_table(&pool, &ctx_a).await;
    create_routing_table(&pool, &ctx_b).await;

    let tbl_a = ctx_a.qualify("_routing_test");
    let tbl_b = ctx_b.qualify("_routing_test");
    for i in 1_i32..=5 {
        sqlx::query(&format!("INSERT INTO {tbl_a} (val) VALUES ({i})"))
            .execute(pool.control())
            .await
            .unwrap();
        sqlx::query(&format!("INSERT INTO {tbl_b} (val) VALUES ({i})"))
            .execute(pool.control())
            .await
            .unwrap();
    }

    let count_a: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {tbl_a}"))
        .fetch_one(pool.control())
        .await
        .unwrap();
    let count_b: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {tbl_b}"))
        .fetch_one(pool.control())
        .await
        .unwrap();
    assert_eq!(count_a, 5, "tenant A must have exactly its own 5 rows");
    assert_eq!(count_b, 5, "tenant B must have exactly its own 5 rows");

    pool.drop_schema(&ctx_a).await.unwrap();
    pool.drop_schema(&ctx_b).await.unwrap();
}

#[tokio::test]
async fn routing_multiple_tables_route_to_same_schema() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx = new_ctx();
    pool.create_schema(&ctx).await.unwrap();

    for table in ["_rt_alpha", "_rt_beta", "_rt_gamma"] {
        let tbl = ctx.qualify(table);
        sqlx::query(&format!("CREATE TABLE IF NOT EXISTS {tbl} (x INT)"))
            .execute(pool.control())
            .await
            .unwrap();
        let schema: String = sqlx::query_scalar(
            "SELECT table_schema FROM information_schema.tables \
             WHERE table_name = $1 AND table_schema = $2",
        )
        .bind(table)
        .bind(ctx.schema_name())
        .fetch_one(pool.control())
        .await
        .unwrap();
        assert_eq!(
            schema,
            ctx.schema_name(),
            "{table} must live in the tenant schema"
        );
    }

    pool.drop_schema(&ctx).await.unwrap();
}

#[tokio::test]
async fn routing_no_cross_contamination_with_equal_row_values() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx_a = new_ctx();
    let ctx_b = new_ctx();
    pool.create_schema(&ctx_a).await.unwrap();
    pool.create_schema(&ctx_b).await.unwrap();
    create_routing_table(&pool, &ctx_a).await;
    create_routing_table(&pool, &ctx_b).await;

    let tbl_a = ctx_a.qualify("_routing_test");
    let tbl_b = ctx_b.qualify("_routing_test");
    sqlx::query(&format!("INSERT INTO {tbl_a} (val) VALUES (42)"))
        .execute(pool.control())
        .await
        .unwrap();
    sqlx::query(&format!("INSERT INTO {tbl_b} (val) VALUES (42)"))
        .execute(pool.control())
        .await
        .unwrap();

    let count_a: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {tbl_a}"))
        .fetch_one(pool.control())
        .await
        .unwrap();
    let count_b: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {tbl_b}"))
        .fetch_one(pool.control())
        .await
        .unwrap();
    assert_eq!(count_a, 1, "tenant A must have exactly 1 row");
    assert_eq!(count_b, 1, "tenant B must have exactly 1 row, not 2");

    pool.drop_schema(&ctx_a).await.unwrap();
    pool.drop_schema(&ctx_b).await.unwrap();
}

#[tokio::test]
async fn routing_bulk_insert_isolated_from_other_tenant() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx_a = new_ctx();
    let ctx_b = new_ctx();
    pool.create_schema(&ctx_a).await.unwrap();
    pool.create_schema(&ctx_b).await.unwrap();
    create_routing_table(&pool, &ctx_a).await;
    create_routing_table(&pool, &ctx_b).await;

    let tbl_a = ctx_a.qualify("_routing_test");
    let values: String = (1_i32..=50)
        .map(|i| format!("({i})"))
        .collect::<Vec<_>>()
        .join(",");
    sqlx::query(&format!("INSERT INTO {tbl_a} (val) VALUES {values}"))
        .execute(pool.control())
        .await
        .unwrap();

    let count_a: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {tbl_a}"))
        .fetch_one(pool.control())
        .await
        .unwrap();
    let count_b: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {}",
        ctx_b.qualify("_routing_test")
    ))
    .fetch_one(pool.control())
    .await
    .unwrap();
    assert_eq!(count_a, 50, "tenant A must have all 50 rows");
    assert_eq!(
        count_b, 0,
        "tenant B must have zero rows after bulk insert into A"
    );

    pool.drop_schema(&ctx_a).await.unwrap();
    pool.drop_schema(&ctx_b).await.unwrap();
}
