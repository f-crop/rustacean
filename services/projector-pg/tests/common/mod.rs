use rb_schemas::TenantId;
use rb_storage_pg::TenantPool;
use rb_tenant::TenantCtx;
use sqlx::postgres::PgPoolOptions;

pub fn test_url() -> Option<String> {
    std::env::var("TEST_DATABASE_URL").ok()
}

pub async fn make_pool(url: &str) -> TenantPool {
    let pg = PgPoolOptions::new()
        .max_connections(3)
        .connect(url)
        .await
        .expect("connect to test DB");
    TenantPool::new(pg)
}

pub fn new_ctx() -> TenantCtx {
    TenantCtx::new(TenantId::new())
}

/// Set up a tenant schema with the `code_tables` migration applied.
pub async fn setup_tenant(pool: &TenantPool, ctx: &TenantCtx) {
    pool.create_schema(ctx).await.expect("create schema");
    let schema = ctx.schema_name();

    sqlx::query(&format!(
        r"CREATE TABLE IF NOT EXISTS {schema}.code_files (
            id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
            repo_id         UUID        NOT NULL,
            relative_path   TEXT        NOT NULL,
            sha256          TEXT        NOT NULL,
            size_bytes      BIGINT      NOT NULL,
            blob_ref        TEXT,
            created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
            updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
            UNIQUE (repo_id, relative_path)
        )"
    ))
    .execute(pool.control())
    .await
    .expect("create code_files");

    sqlx::query(&format!(
        r"CREATE TABLE IF NOT EXISTS {schema}.code_symbols (
            id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
            repo_id         UUID        NOT NULL,
            fqn             TEXT        NOT NULL,
            kind            TEXT        NOT NULL,
            source_path     TEXT,
            line_start      INTEGER,
            line_end        INTEGER,
            blob_ref        TEXT,
            source_text     TEXT,
            created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
            updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
            UNIQUE (repo_id, fqn)
        )"
    ))
    .execute(pool.control())
    .await
    .expect("create code_symbols");

    sqlx::query(&format!(
        r"CREATE TABLE IF NOT EXISTS {schema}.code_relations (
            id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
            repo_id         UUID        NOT NULL,
            from_fqn        TEXT        NOT NULL,
            to_fqn          TEXT        NOT NULL,
            kind            TEXT        NOT NULL,
            created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
            UNIQUE (repo_id, from_fqn, to_fqn, kind)
        )"
    ))
    .execute(pool.control())
    .await
    .expect("create code_relations");
}

pub async fn teardown_tenant(pool: &TenantPool, ctx: &TenantCtx) {
    let _ = pool.drop_schema(ctx).await;
}
