//! Integration tests for ingestion-run activity timestamps (started_at backfill).
//!
//! These tests require a running Postgres instance accessible via
//! `RB_DATABASE_URL`. When that variable is absent the tests skip gracefully.

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

async fn real_pool() -> Option<PgPool> {
    let db_url = std::env::var("RB_DATABASE_URL").ok()?;
    PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .ok()
}

/// AC: `fail_run` sets `started_at = COALESCE(started_at, now())` so that runs
/// which fail before any Processing event is consumed always have a non-NULL
/// `started_at` and a non-zero measurable duration.
#[tokio::test]
async fn fail_run_sets_started_at_when_null() {
    let Some(pool) = real_pool().await else {
        return; // skip: no DB
    };

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();

    let slug = format!("act1596f-{}", tenant_id.simple());
    let schema_name = format!("act1596f_{}", tenant_id.simple());

    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(&slug)
    .bind("Activity 1596 Fail Test Tenant")
    .bind(&schema_name)
    .execute(&pool)
    .await
    .expect("insert tenant");

    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, email_verified_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(user_id)
    .bind(format!("act1596f-{}@test.example", user_id.simple()))
    .bind("$argon2id$v=19$m=65536,t=1,p=1$placeholder_hash")
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

    sqlx::query(
        "INSERT INTO control.repos \
         (id, tenant_id, github_repo_id, full_name, default_branch, connected_by) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(repo_id)
    .bind(tenant_id)
    .bind(15961_i64)
    .bind("test-org/act1596f-repo")
    .bind("main")
    .bind(user_id)
    .execute(&pool)
    .await
    .expect("insert repo");

    // Run starts in 'queued' with started_at = NULL (no Processing event received yet).
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

    // Verify precondition: started_at is NULL before fail.
    let started_before: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT started_at FROM control.ingestion_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .expect("fetch started_at before");
    assert!(
        started_before.is_none(),
        "precondition: started_at must be NULL before fail_run fires"
    );

    // Execute the fail_run SQL (mirrors ingest_consumer/db.rs fail_run).
    sqlx::query(
        "UPDATE control.ingestion_runs \
         SET status = 'failed', \
             started_at = COALESCE(started_at, now()), \
             finished_at = now(), \
             error = $2 \
         WHERE id = $1 AND status IN ('queued', 'running')",
    )
    .bind(run_id)
    .bind(Some("clone_failed: test error"))
    .execute(&pool)
    .await
    .expect("fail_run SQL");

    let (started_at, finished_at): (
        Option<chrono::DateTime<chrono::Utc>>,
        Option<chrono::DateTime<chrono::Utc>>,
    ) = sqlx::query_as("SELECT started_at, finished_at FROM control.ingestion_runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .expect("fetch after fail");

    let started = started_at.expect("started_at must be non-NULL after fail_run");
    let finished = finished_at.expect("finished_at must be non-NULL after fail_run");
    assert!(
        started <= finished,
        "started_at must be <= finished_at; got started={started}, finished={finished}"
    );
}

/// AC: `maybe_complete_run` sets `started_at = COALESCE(started_at, now())` so
/// runs that complete before any Processing event is processed always have a
/// non-NULL `started_at`.
#[tokio::test]
async fn complete_run_sets_started_at_when_null() {
    let Some(pool) = real_pool().await else {
        return; // skip: no DB
    };

    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();

    let slug = format!("act1596c-{}", tenant_id.simple());
    let schema_name = format!("act1596c_{}", tenant_id.simple());

    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(&slug)
    .bind("Activity 1596 Complete Test Tenant")
    .bind(&schema_name)
    .execute(&pool)
    .await
    .expect("insert tenant");

    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, email_verified_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(user_id)
    .bind(format!("act1596c-{}@test.example", user_id.simple()))
    .bind("$argon2id$v=19$m=65536,t=1,p=1$placeholder_hash")
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

    sqlx::query(
        "INSERT INTO control.repos \
         (id, tenant_id, github_repo_id, full_name, default_branch, connected_by) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(repo_id)
    .bind(tenant_id)
    .bind(15962_i64)
    .bind("test-org/act1596c-repo")
    .bind("main")
    .bind(user_id)
    .execute(&pool)
    .await
    .expect("insert repo");

    // Run starts in 'queued' with started_at = NULL (Processing event not yet seen).
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

    // Insert all 9 stages as succeeded with staggered timestamps.
    // The last stage finishes at now()+1s so MAX(stage.finished_at) > now(),
    // ensuring COALESCE(started_at, now()) <= finished_at after both UPDATEs.
    let stages: &[(&str, &str)] = &[
        ("clone", "now() - interval '8 seconds'"),
        ("expand", "now() - interval '7 seconds'"),
        ("parse", "now() - interval '6 seconds'"),
        ("typecheck", "now() - interval '5 seconds'"),
        ("extract", "now() - interval '4 seconds'"),
        ("embed", "now() - interval '3 seconds'"),
        ("project_pg", "now() - interval '2 seconds'"),
        ("project_neo4j", "now() - interval '1 second'"),
        ("project_qdrant", "now() + interval '1 second'"),
    ];
    for (stage, ts_expr) in stages {
        let sql = format!(
            "INSERT INTO control.pipeline_stage_runs \
             (id, ingestion_run_id, stage, status, finished_at) \
             VALUES (gen_random_uuid(), $1, $2, 'succeeded', {ts_expr})"
        );
        sqlx::query(&sql)
            .bind(run_id)
            .bind(*stage)
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("insert stage {stage}: {e}"));
    }

    // Execute the first UPDATE from maybe_complete_run (mirrors ingest_consumer/db.rs).
    sqlx::query(
        "UPDATE control.ingestion_runs \
         SET status = 'succeeded', started_at = COALESCE(started_at, now()) \
         WHERE id = $1 AND status IN ('queued', 'running')",
    )
    .bind(run_id)
    .execute(&pool)
    .await
    .expect("status transition");

    // Execute the second UPDATE (advance finished_at).
    sqlx::query(
        "UPDATE control.ingestion_runs \
         SET finished_at = (\
           SELECT MAX(psr.finished_at) \
           FROM control.pipeline_stage_runs psr \
           WHERE psr.ingestion_run_id = $1\
         ) \
         WHERE id = $1 AND status = 'succeeded'",
    )
    .bind(run_id)
    .execute(&pool)
    .await
    .expect("advance finished_at");

    let (started_at, finished_at, created_at): (
        Option<chrono::DateTime<chrono::Utc>>,
        Option<chrono::DateTime<chrono::Utc>>,
        chrono::DateTime<chrono::Utc>,
    ) = sqlx::query_as(
        "SELECT started_at, finished_at, created_at FROM control.ingestion_runs WHERE id = $1",
    )
    .bind(run_id)
    .fetch_one(&pool)
    .await
    .expect("fetch after complete");

    let started = started_at.expect("started_at must be non-NULL after maybe_complete_run");
    let finished = finished_at.expect("finished_at must be non-NULL after maybe_complete_run");

    assert!(
        started <= finished,
        "started_at must be <= finished_at; got started={started}, finished={finished}"
    );
    assert!(
        finished > created_at,
        "finished_at must be > created_at (non-zero duration); \
         got created={created_at}, finished={finished}"
    );
}
