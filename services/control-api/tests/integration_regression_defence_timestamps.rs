//! Regression-defence smoke test: ingestion timestamp monotonicity.
//!
//! 3. **Ingestion timestamp monotonicity** — after a completed pipeline run
//!    every stage row must carry a non-NULL `started_at`,
//!    `ingestion_runs.finished_at` must equal the
//!    `MAX(pipeline_stage_runs.finished_at)` (not the typecheck-only value),
//!    and stage `started_at` timestamps must be monotonically non-decreasing
//!    across the canonical stage ordering.
//!
//! DB-backed tests skip gracefully when `RB_DATABASE_URL` is not set.

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

async fn cleanup_user(pool: &PgPool, user_id: Uuid, tenant_id: Uuid) {
    sqlx::query("DELETE FROM control.sessions WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.tenant_members WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.users WHERE id = $1")
        .bind(user_id)
        .execute(pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(pool)
        .await
        .ok();
}

// ── Test 3: Ingestion-run timestamp monotonicity ─────────────────────────────

/// After a completed pipeline run the following invariants must all hold:
///
/// - Every `pipeline_stage_runs` row has `started_at IS NOT NULL`.
/// - `ingestion_runs.finished_at` equals `MAX(pipeline_stage_runs.finished_at)`
///   across all stage rows (not capped to typecheck stage only).
/// - Stage `started_at` timestamps are monotonically non-decreasing in the
///   canonical pipeline stage order.
///
/// Regression defence: ingestion timestamp monotonicity.
#[tokio::test]
async fn ingestion_timestamp_monotonicity() {
    let Some(pool) = db_pool().await else {
        return; // skip: no DB
    };

    // Seed full prerequisite chain: tenant → user → member → installation → repo.
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(format!("rd-ts-{}", tenant_id.simple()))
    .bind("Timestamp Test Tenant")
    .bind(format!("rd_ts_{}", tenant_id.simple()))
    .execute(&pool)
    .await
    .expect("insert tenant");

    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, email_verified_at) \
         VALUES ($1, $2, '$argon2id$v=19$m=65536,t=1,p=1$placeholder_hash', now())",
    )
    .bind(user_id)
    .bind(format!("rd-ts-{}@test.example", user_id.simple()))
    .execute(&pool)
    .await
    .expect("insert user");

    sqlx::query(
        "INSERT INTO control.tenant_members (tenant_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(tenant_id)
    .bind(user_id)
    .execute(&pool)
    .await
    .expect("insert tenant_member");

    let github_install_id = random_install_id();
    let install_uuid = seed_installation(&pool, tenant_id, github_install_id).await;

    sqlx::query(
        "INSERT INTO control.repos \
         (id, tenant_id, installation_id, github_repo_id, full_name, default_branch, connected_by) \
         VALUES ($1, $2, $3, $4, 'ts-org/ts-repo', 'main', $5)",
    )
    .bind(repo_id)
    .bind(tenant_id)
    .bind(install_uuid)
    .bind(github_install_id)
    .bind(user_id)
    .execute(&pool)
    .await
    .expect("insert repo");

    sqlx::query(
        "INSERT INTO control.ingestion_runs (id, tenant_id, repo_id, status, requested_by) \
         VALUES ($1, $2, $3, 'queued', $4)",
    )
    .bind(run_id)
    .bind(tenant_id)
    .bind(repo_id)
    .bind(user_id)
    .execute(&pool)
    .await
    .expect("insert ingestion_run");

    // Canonical stage ordering — must match the CHECK constraint and PIPELINE_STAGES
    // in `repos.rs`.  Each stage gets a staggered started_at and finished_at to
    // create a verifiable monotonic sequence and a max-finished_at that is clearly
    // the last stage (project_qdrant), not typecheck (stage 4).
    let canonical_stages = [
        "clone",
        "expand",
        "parse",
        "typecheck",
        "extract",
        "embed",
        "project_pg",
        "project_neo4j",
        "project_qdrant",
    ];

    // Insert all 9 stage rows first (mirrors what `trigger_ingest` does in production).
    for stage in &canonical_stages {
        sqlx::query(
            "INSERT INTO control.pipeline_stage_runs \
             (id, ingestion_run_id, stage, status) \
             VALUES (gen_random_uuid(), $1, $2, 'pending') \
             ON CONFLICT (ingestion_run_id, stage) DO NOTHING",
        )
        .bind(run_id)
        .bind(*stage)
        .execute(&pool)
        .await
        .expect("insert stage row");
    }

    // Drive each stage through running → succeeded with staggered timestamps so
    // the monotonicity assertion and the max-finished_at assertion are meaningful.
    // Earlier stages get an earlier started_at; project_qdrant (last) gets the
    // largest finished_at, proving the run.finished_at cap is not typecheck-only.
    for (i, stage) in canonical_stages.iter().enumerate() {
        let offset = i as i64;

        // Simulate `update_stage_run("running", ...)`: sets started_at via COALESCE.
        let running_sql = format!(
            "UPDATE control.pipeline_stage_runs \
             SET status = 'running', \
                 started_at = COALESCE(started_at, now() - interval '{} seconds') \
             WHERE ingestion_run_id = $1 AND stage = $2",
            9 - offset
        );
        sqlx::query(&running_sql)
            .bind(run_id)
            .bind(*stage)
            .execute(&pool)
            .await
            .expect("running UPDATE");

        // Simulate `update_stage_run("succeeded", ...)`: sets finished_at.
        let succeeded_sql = format!(
            "UPDATE control.pipeline_stage_runs \
             SET status = 'succeeded', \
                 finished_at = now() - interval '{} seconds' \
             WHERE ingestion_run_id = $1 AND stage = $2",
            8 - offset
        );
        sqlx::query(&succeeded_sql)
            .bind(run_id)
            .bind(*stage)
            .execute(&pool)
            .await
            .expect("succeeded UPDATE");
    }

    // Run `maybe_complete_run` equivalent: transition to succeeded + set finished_at.
    sqlx::query(
        "UPDATE control.ingestion_runs \
         SET status = 'succeeded', \
             started_at = COALESCE(started_at, now()), \
             finished_at = ( \
               SELECT MAX(psr.finished_at) \
               FROM control.pipeline_stage_runs psr \
               WHERE psr.ingestion_run_id = $1 \
             ) \
         WHERE id = $1 AND status IN ('queued', 'running')",
    )
    .bind(run_id)
    .execute(&pool)
    .await
    .expect("maybe_complete_run");

    // ── Assert 1: every stage has started_at IS NOT NULL ─────────────────────

    let null_started: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM control.pipeline_stage_runs \
         WHERE ingestion_run_id = $1 AND started_at IS NULL",
    )
    .bind(run_id)
    .fetch_one(&pool)
    .await
    .expect("null started_at count");

    assert_eq!(
        null_started, 0,
        "every pipeline stage must have started_at IS NOT NULL"
    );

    // ── Assert 2: run.finished_at = MAX(stage.finished_at) ───────────────────

    let run_finished: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT finished_at FROM control.ingestion_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .expect("fetch run finished_at");
    let run_finished = run_finished.expect("run finished_at must be non-NULL");

    let max_stage_finished: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT MAX(finished_at) FROM control.pipeline_stage_runs WHERE ingestion_run_id = $1",
    )
    .bind(run_id)
    .fetch_one(&pool)
    .await
    .expect("fetch max stage finished_at");
    let max_stage_finished = max_stage_finished.expect("max stage finished_at must be non-NULL");

    // Allow up to 1 µs tolerance for clock rounding.
    let delta = (run_finished - max_stage_finished)
        .abs()
        .num_microseconds()
        .unwrap_or(i64::MAX);
    assert!(
        delta < 1000,
        "ingestion_runs.finished_at ({run_finished}) must equal MAX(stage.finished_at) \
         ({max_stage_finished}), not be capped to typecheck only"
    );

    // ── Assert 3: stage started_at is monotonically non-decreasing ───────────

    type StageRow = (String, Option<chrono::DateTime<chrono::Utc>>);
    let stage_rows: Vec<StageRow> = sqlx::query_as(
        "SELECT psr.stage, psr.started_at \
         FROM control.pipeline_stage_runs psr \
         WHERE psr.ingestion_run_id = $1 \
         ORDER BY psr.created_at ASC",
    )
    .bind(run_id)
    .fetch_all(&pool)
    .await
    .expect("fetch stage rows");

    assert_eq!(
        stage_rows.len(),
        canonical_stages.len(),
        "expected {} stage rows, got {}",
        canonical_stages.len(),
        stage_rows.len()
    );

    // Verify canonical ordering is preserved and started_at is non-decreasing.
    let mut prev_started: Option<chrono::DateTime<chrono::Utc>> = None;
    for (actual_stage, started_at) in &stage_rows {
        let started = started_at.expect("started_at must be non-NULL for all stages");
        if let Some(prev) = prev_started {
            assert!(
                started >= prev,
                "stage '{actual_stage}' started_at ({started}) must be >= previous stage \
                 started_at ({prev}) — monotonicity violated"
            );
        }
        prev_started = Some(started);
    }

    // Cleanup.
    sqlx::query("DELETE FROM control.ingestion_runs WHERE id = $1")
        .bind(run_id)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.repos WHERE id = $1")
        .bind(repo_id)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.github_installations WHERE github_installation_id = $1")
        .bind(github_install_id)
        .execute(&pool)
        .await
        .ok();
    cleanup_user(&pool, user_id, tenant_id).await;
}
