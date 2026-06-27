//! Retrieval latency load-test fixture (ADR-014 §9, Wave 10 S7).
//!
//! Asserts that the Postgres FTS leg of the hybrid retrieval path meets its
//! latency budget when run against a warm tenant schema with synthetic data.
//!
//! # Skip behaviour
//!
//! All tests skip when `RB_DATABASE_URL` is not set, matching the project
//! convention for integration tests that require a live Postgres instance.
//!
//! # Budget (ADR-014 §9, FTS-leg only)
//!
//! The FTS leg must be well under the hybrid p50/p95/p99 budget so that the
//! full hybrid path (FTS + ANN + RRF) fits within the total budget when the
//! dense leg returns quickly from Qdrant.  This test asserts a conservative
//! sub-budget for the FTS leg alone.
//!
//! | Mode        | p50     | p95     | p99     |
//! |-------------|---------|---------|---------|
//! | FTS leg     | ≤80 ms  | ≤200 ms | ≤350 ms |
//! | Full hybrid | ≤180 ms | ≤400 ms | ≤700 ms |

use std::time::{Duration, Instant};
use uuid::Uuid;

fn db_url() -> Option<String> {
    std::env::var("RB_DATABASE_URL").ok()
}

/// Compute the p-th percentile of a sorted slice (0..=100).
fn percentile(sorted: &[Duration], p: u8) -> Duration {
    assert!(!sorted.is_empty());
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let idx = ((f64::from(p) / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

async fn seed_symbols(
    pool: &sqlx::PgPool,
    tenant_schema: &str,
    repo_id: Uuid,
    n: usize,
) -> Result<(), sqlx::Error> {
    for i in 0..n {
        let fqn = format!("test_crate::module_{i}::Symbol{i}");
        let source_text = format!("pub fn symbol_{i}() {{ /* body */ }}");
        sqlx::query(&format!(
            "INSERT INTO {tenant_schema}.code_symbols \
             (id, repo_id, fqn, kind, source_path, line_start, line_end, source_text, \
              embedding_model, blob_ref) \
             VALUES ($1, $2, $3, 'fn', 'src/lib.rs', {i}, {end}, $4, 'nomic-embed-text', NULL) \
             ON CONFLICT (repo_id, fqn) DO NOTHING",
            end = i + 5,
        ))
        .bind(Uuid::new_v4())
        .bind(repo_id)
        .bind(&fqn)
        .bind(&source_text)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// AC6 — FTS leg latency within the hybrid p50/p95/p99 sub-budget.
///
/// Seeds 500 rows, warms the GIN index, then runs 60 FTS queries and asserts
/// percentiles are within the conservative per-leg budget.
#[tokio::test]
async fn fts_leg_meets_hybrid_budget() {
    const SYMBOLS: usize = 500;
    const ITERATIONS: usize = 60;
    const P50_LIMIT: Duration = Duration::from_millis(80);
    const P95_LIMIT: Duration = Duration::from_millis(200);
    const P99_LIMIT: Duration = Duration::from_millis(350);

    let Some(db_url) = db_url() else {
        eprintln!("RB_DATABASE_URL not set — skipping FTS latency test");
        return;
    };

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&db_url)
        .await
        .expect("connect to test db");

    let tenant_id = Uuid::new_v4();
    let tenant_schema = format!("perf_tenant_{}", tenant_id.simple());
    let repo_id = Uuid::new_v4();

    sqlx::query(&format!("CREATE SCHEMA IF NOT EXISTS {tenant_schema}"))
        .execute(&pool)
        .await
        .expect("create tenant schema");

    sqlx::query(&format!(
        "CREATE TABLE IF NOT EXISTS {tenant_schema}.code_symbols ( \
         id UUID PRIMARY KEY DEFAULT gen_random_uuid(), \
         repo_id UUID NOT NULL, \
         fqn TEXT NOT NULL, \
         kind TEXT NOT NULL, \
         source_path TEXT, \
         line_start INT, \
         line_end INT, \
         source_text TEXT, \
         embedding_model TEXT, \
         blob_ref TEXT, \
         UNIQUE (repo_id, fqn) \
         )"
    ))
    .execute(&pool)
    .await
    .expect("create code_symbols table");

    let _ = sqlx::query(&format!(
        "ALTER TABLE {tenant_schema}.code_symbols \
         ADD COLUMN IF NOT EXISTS fts tsvector \
         GENERATED ALWAYS AS ( \
             to_tsvector('simple', coalesce(fqn,'') || ' ' || coalesce(source_text,'')) \
         ) STORED"
    ))
    .execute(&pool)
    .await;

    let _ = sqlx::query(&format!(
        "CREATE INDEX IF NOT EXISTS code_symbols_fts_gin \
         ON {tenant_schema}.code_symbols USING gin(fts)"
    ))
    .execute(&pool)
    .await;

    seed_symbols(&pool, &tenant_schema, repo_id, SYMBOLS)
        .await
        .expect("seed symbols");

    // Warm index.
    let _ = sqlx::query(&format!(
        "SELECT fqn FROM {tenant_schema}.code_symbols \
         WHERE fts @@ plainto_tsquery('simple', 'symbol') LIMIT 50"
    ))
    .execute(&pool)
    .await;

    let mut durations: Vec<Duration> = Vec::with_capacity(ITERATIONS);
    for i in 0..ITERATIONS {
        let q = format!("Symbol{}", i % 50);
        let t0 = Instant::now();
        let _rows: Vec<(String,)> = sqlx::query_as(&format!(
            "SELECT fqn FROM {tenant_schema}.code_symbols \
             WHERE fts @@ plainto_tsquery('simple', $1) \
             ORDER BY ts_rank_cd(fts, plainto_tsquery('simple', $1)) DESC \
             LIMIT 50"
        ))
        .bind(&q)
        .fetch_all(&pool)
        .await
        .expect("FTS query");
        durations.push(t0.elapsed());
    }

    durations.sort_unstable();

    let p50 = percentile(&durations, 50);
    let p95 = percentile(&durations, 95);
    let p99 = percentile(&durations, 99);

    eprintln!("FTS latency — p50={p50:?} p95={p95:?} p99={p99:?}");

    let _ = sqlx::query(&format!("DROP SCHEMA {tenant_schema} CASCADE"))
        .execute(&pool)
        .await;

    assert!(
        p50 <= P50_LIMIT,
        "FTS p50 {p50:?} exceeds budget {P50_LIMIT:?}"
    );
    assert!(
        p95 <= P95_LIMIT,
        "FTS p95 {p95:?} exceeds budget {P95_LIMIT:?}"
    );
    assert!(
        p99 <= P99_LIMIT,
        "FTS p99 {p99:?} exceeds budget {P99_LIMIT:?}"
    );
}

/// AC6 — Multi-query N config default is 1 (disabled; S5 controls the ceiling via
/// `rb_query::MAX_MULTI_QUERY_N`). Exercises the `Config::for_test()` default.
#[test]
fn multi_query_n_default_is_one() {
    let cfg = control_api::Config::for_test();
    assert_eq!(cfg.multi_query_n, 1);
}

/// AC6 — Rerank candidate cap config default is 50.
#[test]
fn rerank_candidate_cap_default_is_fifty() {
    let cfg = control_api::Config::for_test();
    assert_eq!(cfg.rerank_candidate_cap, 50);
}

/// AC7 — LLM token ceiling default is 0 (zero LLM cost for all new tenants).
#[test]
fn llm_token_ceiling_default_is_zero() {
    let cfg = control_api::Config::for_test();
    assert_eq!(
        cfg.llm_token_ceiling_per_tenant, 0,
        "default must be 0 so brand-new tenants have zero LLM cost"
    );
}
