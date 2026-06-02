use std::sync::Arc;
use std::time::Duration;

use rb_auth::{LoginRateLimiter, PasswordHasher};
use rb_email::from_transport;
use rb_sse::{EventBus, SseConfig};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use super::*;
use crate::{
    AppState, Config, KafkaConsistencyState, McpSessionStore, SessionCreateRateLimiter,
    TenantSessionCount, state::AgentRegistry,
};

/// Connect to the real Postgres instance.
/// Returns `None` when `RB_DATABASE_URL` is absent — callers skip gracefully.
async fn test_pool() -> Option<PgPool> {
    let url = std::env::var("RB_DATABASE_URL").ok()?;
    PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .ok()
}

/// Insert the minimal control-schema rows needed to satisfy FK constraints
/// on `ingestion_runs`: tenant → user → `github_installation` → repo.
/// Returns `(tenant_id, user_id, repo_id)`.
async fn insert_fixtures(pool: &PgPool) -> (Uuid, Uuid, Uuid) {
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let install_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(format!("rec-test-{}", tenant_id.simple()))
    .bind("Reconciler Test Tenant")
    .bind(format!("rec_{}", tenant_id.simple()))
    .execute(pool)
    .await
    .expect("insert tenant");

    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, email_verified_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(user_id)
    .bind(format!("rec-{}@test.example", user_id.simple()))
    .bind("$argon2id$v=19$m=65536,t=1,p=1$placeholder_hash")
    .execute(pool)
    .await
    .expect("insert user");

    sqlx::query(
        "INSERT INTO control.tenant_members (tenant_id, user_id, role) \
         VALUES ($1, $2, 'owner')",
    )
    .bind(tenant_id)
    .bind(user_id)
    .execute(pool)
    .await
    .expect("insert tenant_member");

    let github_install_id = i64::from_ne_bytes(install_id.as_bytes()[0..8].try_into().unwrap());
    sqlx::query(
        "INSERT INTO control.github_installations \
         (id, tenant_id, github_installation_id, account_login, account_type, account_id) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(install_id)
    .bind(tenant_id)
    .bind(github_install_id)
    .bind("test-org")
    .bind("Organization")
    .bind(42_i64)
    .execute(pool)
    .await
    .expect("insert github_installation");

    let github_repo_id = i64::from_ne_bytes(repo_id.as_bytes()[0..8].try_into().unwrap());
    sqlx::query(
        "INSERT INTO control.repos \
         (id, tenant_id, installation_id, github_repo_id, full_name, default_branch, connected_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(repo_id)
    .bind(tenant_id)
    .bind(install_id)
    .bind(github_repo_id)
    .bind("test-org/test-repo")
    .bind("main")
    .bind(user_id)
    .execute(pool)
    .await
    .expect("insert repo");

    (tenant_id, user_id, repo_id)
}

/// Insert a `queued` ingestion run.
/// `old = true` → `created_at = now() - 10 minutes` (past the 5-min threshold).
/// `old = false` → `created_at = now() - 1 minute` (within threshold; not an orphan).
async fn insert_queued_run(
    pool: &PgPool,
    tenant_id: Uuid,
    repo_id: Uuid,
    user_id: Uuid,
    old: bool,
) -> Uuid {
    let run_id = Uuid::new_v4();
    let sql = if old {
        "INSERT INTO control.ingestion_runs \
         (id, tenant_id, repo_id, status, requested_by, created_at) \
         VALUES ($1, $2, $3, 'queued', $4, now() - interval '10 minutes')"
    } else {
        "INSERT INTO control.ingestion_runs \
         (id, tenant_id, repo_id, status, requested_by, created_at) \
         VALUES ($1, $2, $3, 'queued', $4, now() - interval '1 minute')"
    };
    sqlx::query(sql)
        .bind(run_id)
        .bind(tenant_id)
        .bind(repo_id)
        .bind(user_id)
        .execute(pool)
        .await
        .expect("insert ingestion_run");
    run_id
}

/// Insert a `queued` run with `started_at` already set — simulates a run
/// already claimed by a previous reconciler pass or the clone worker.
async fn insert_claimed_run(pool: &PgPool, tenant_id: Uuid, repo_id: Uuid, user_id: Uuid) -> Uuid {
    let run_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO control.ingestion_runs \
         (id, tenant_id, repo_id, status, requested_by, created_at, started_at) \
         VALUES ($1, $2, $3, 'queued', $4, now() - interval '10 minutes', now() - interval '1 minute')",
    )
    .bind(run_id)
    .bind(tenant_id)
    .bind(repo_id)
    .bind(user_id)
    .execute(pool)
    .await
    .expect("insert claimed run");
    run_id
}

/// Build an `AppState` with a real Postgres pool and no Kafka producer.
fn make_state(pool: PgPool) -> AppState {
    let db_url = std::env::var("RB_DATABASE_URL").unwrap_or_default();
    let smtp = rb_email::SmtpConfig {
        host: String::new(),
        port: 587,
        username: String::new(),
        password: String::new(),
        from_address: "test@example.com".to_owned(),
    };
    let email_sender = from_transport("noop", &smtp).expect("noop transport");
    let hasher = PasswordHasher::from_config(64, 1, 1).expect("hasher");
    let config = Config {
        listen_addr: "127.0.0.1:0".to_owned(),
        database_url: db_url,
        cors_origins: vec![],
        base_url: "http://localhost".to_owned(),
        session_ttl_days: 30,
        argon2_memory_kb: 64,
        argon2_time_cost: 1,
        argon2_parallelism: 1,
        email_transport: "noop".to_owned(),
        service_name: "jobs-test".to_owned(),
        secure_cookies: false,
        gh_app_id: None,
        gh_app_private_key_b64: None,
        gh_app_webhook_secret: None,
        gh_app_enc_key_b64: None,
        gh_api_base: rb_github::DEFAULT_GITHUB_API_BASE.to_owned(),
        neo4j_uri: None,
        neo4j_user: "neo4j".to_owned(),
        neo4j_password: None,
        kafka_bootstrap_servers: "127.0.0.1:19999".to_owned(),
        dev_test_routes: false,
        migrations_root: None,
        qdrant_url: None,
        ollama_url: None,
        embedding_model: "nomic-embed-text".to_owned(),
        internal_secret: Some("test".to_owned()),
        internal_listen_addr: "127.0.0.1:0".to_owned(),
        session_create_rate_limit: 10,
        session_create_window_secs: 60,
        tenant_session_cap: 100,
        admin_token: None,
        chat_panel_enabled: false,
        tempo_base_url: "http://localhost:3000".to_owned(),
        mcp_jwt_secret: Some("test-mcp-jwt-secret".to_owned()),
        mcp_jwt_ttl_secs: 900,
        llm_api_key: None,
    };
    AppState {
        pool,
        email_sender: Arc::from(email_sender),
        hasher: Arc::new(hasher),
        login_rate_limiter: Arc::new(LoginRateLimiter::new()),
        config: Arc::new(config),
        gh_loader: Arc::new(rb_github::GhAppLoader::new(None)),
        sse_bus: Arc::new(EventBus::new(SseConfig::default())),
        ingest_producer: None,
        tombstone_producer: None,
        module_tree_cache: rb_query::new_module_tree_cache(),
        graph: None,
        qdrant: None,
        http_client: reqwest::Client::new(),
        neo4j_uri: None,
        kafka_consistency: Arc::new(KafkaConsistencyState::new()),
        mcp_sessions: McpSessionStore::new(),
        agent_registry: AgentRegistry::new(),
        agent_commands_producer: None,
        internal_secret: "test".to_owned(),
        session_create_rate_limiter: Arc::new(SessionCreateRateLimiter::default()),
        tenant_session_count: Arc::new(TenantSessionCount::new()),
        mcp_jwt_secret: "test-mcp-jwt-secret".to_owned(),
        mcp_jwt_ttl_secs: 900,
    }
}

fn make_state_with_dead_producer(pool: PgPool) -> AppState {
    let cfg = rb_kafka::ProducerCfg {
        bootstrap_servers: "127.0.0.1:19999".to_owned(),
        compression_type: "none".to_owned(),
        linger_ms: 0,
        delivery_timeout_ms: 1_000,
        queue_buffering_max_kbytes: 1_024,
    };
    let producer = rb_kafka::Producer::new(&cfg).expect("create dead producer");
    let mut state = make_state(pool);
    state.ingest_producer = Some(Arc::new(producer));
    state
}

// -----------------------------------------------------------------------
// Behavioral tests — require RB_DATABASE_URL; skip gracefully when absent
// -----------------------------------------------------------------------

#[tokio::test]
async fn fetch_returns_old_queued_runs() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let (tenant_id, user_id, repo_id) = insert_fixtures(&pool).await;
    let run_id = insert_queued_run(&pool, tenant_id, repo_id, user_id, true).await;

    let runs = fetch_orphaned_runs(&pool).await.expect("fetch");
    assert!(
        runs.iter().any(|r| r.id == run_id),
        "old queued run must appear in orphan results"
    );
}

#[tokio::test]
async fn fetch_ignores_recent_queued_runs() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let (tenant_id, user_id, repo_id) = insert_fixtures(&pool).await;
    let run_id = insert_queued_run(&pool, tenant_id, repo_id, user_id, false).await;

    let runs = fetch_orphaned_runs(&pool).await.expect("fetch");
    assert!(
        !runs.iter().any(|r| r.id == run_id),
        "recent queued run must NOT appear in orphan results"
    );
}

#[tokio::test]
async fn fetch_ignores_non_queued_runs() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let (tenant_id, user_id, repo_id) = insert_fixtures(&pool).await;
    let run_id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO control.ingestion_runs \
         (id, tenant_id, repo_id, status, requested_by, created_at) \
         VALUES ($1, $2, $3, 'failed', $4, now() - interval '10 minutes')",
    )
    .bind(run_id)
    .bind(tenant_id)
    .bind(repo_id)
    .bind(user_id)
    .execute(&pool)
    .await
    .expect("insert failed run");

    let runs = fetch_orphaned_runs(&pool).await.expect("fetch");
    assert!(
        !runs.iter().any(|r| r.id == run_id),
        "non-queued run must NOT appear in orphan results"
    );
}

#[tokio::test]
async fn claim_run_is_exclusive() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let (tenant_id, user_id, repo_id) = insert_fixtures(&pool).await;
    let run_id = insert_queued_run(&pool, tenant_id, repo_id, user_id, true).await;

    let first = try_claim_run(&pool, run_id).await.expect("claim 1");
    let second = try_claim_run(&pool, run_id).await.expect("claim 2");

    assert!(first, "first claim must succeed");
    assert!(!second, "second claim must fail — run already claimed");
}

#[tokio::test]
async fn mark_failed_transitions_queued_to_failed() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let (tenant_id, user_id, repo_id) = insert_fixtures(&pool).await;
    let run_id = insert_queued_run(&pool, tenant_id, repo_id, user_id, true).await;

    mark_failed(&pool, run_id, "test error").await;

    let status: String =
        sqlx::query_scalar("SELECT status FROM control.ingestion_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .expect("fetch status");
    assert_eq!(status, "failed");
}

#[tokio::test]
async fn reconcile_no_producer_marks_orphans_failed() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let (tenant_id, user_id, repo_id) = insert_fixtures(&pool).await;
    let run_id = insert_queued_run(&pool, tenant_id, repo_id, user_id, true).await;

    let state = make_state(pool.clone());
    // ingest_producer is None — exercises the no-Kafka path.
    reconcile_orphaned_ingest_runs(&state).await;

    let status: String =
        sqlx::query_scalar("SELECT status FROM control.ingestion_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .expect("fetch status");
    assert_eq!(
        status, "failed",
        "orphaned run must be marked failed when no Kafka producer is available"
    );
}

/// A `queued` run with `started_at` set must not appear in orphan results.
/// Verifies that `AND started_at IS NULL` in `fetch_orphaned_runs` prevents
/// double-dispatch when a previous reconciler pass already claimed the row.
#[tokio::test]
async fn fetch_ignores_claimed_runs() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let (tenant_id, user_id, repo_id) = insert_fixtures(&pool).await;
    let run_id = insert_claimed_run(&pool, tenant_id, repo_id, user_id).await;

    let runs = fetch_orphaned_runs(&pool).await.expect("fetch");
    assert!(
        !runs.iter().any(|r| r.id == run_id),
        "claimed run (started_at IS NOT NULL) must NOT appear in orphan results"
    );
}

/// Full reconcile with a pre-claimed run: the reconciler must leave the row
/// untouched (neither re-publish nor transition it to `failed`).
#[tokio::test]
async fn reconcile_skips_already_claimed_run() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let (tenant_id, user_id, repo_id) = insert_fixtures(&pool).await;
    let run_id = insert_claimed_run(&pool, tenant_id, repo_id, user_id).await;

    let state = make_state(pool.clone());
    reconcile_orphaned_ingest_runs(&state).await;

    let (status, started_at): (String, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT status, started_at FROM control.ingestion_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .expect("fetch");

    assert_eq!(status, "queued", "pre-claimed run must remain queued");
    assert!(
        started_at.is_some(),
        "started_at must remain set after skip"
    );
}

/// Verify `LIMIT 100` bound: when more than 100 eligible orphaned runs exist,
/// `fetch_orphaned_runs` returns at most 100.
#[tokio::test]
async fn fetch_respects_limit() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let (tenant_id, user_id, repo_id) = insert_fixtures(&pool).await;

    // Seed 101 orphaned runs — one past the LIMIT 100 bound.
    for _ in 0..101_u32 {
        insert_queued_run(&pool, tenant_id, repo_id, user_id, true).await;
    }

    let runs = fetch_orphaned_runs(&pool).await.expect("fetch");
    assert!(
        runs.len() <= 100,
        "fetch_orphaned_runs must return at most 100 rows (returned {})",
        runs.len()
    );
}

/// A producer that is configured but whose broker is unreachable (`check_ready`
/// returns false) must cause orphaned runs to be marked `failed`, not left
/// in `queued`.
#[tokio::test]
async fn reconcile_dead_broker_marks_orphans_failed() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let (tenant_id, user_id, repo_id) = insert_fixtures(&pool).await;
    let run_id = insert_queued_run(&pool, tenant_id, repo_id, user_id, true).await;

    let state = make_state_with_dead_producer(pool.clone());
    reconcile_orphaned_ingest_runs(&state).await;

    let status: String =
        sqlx::query_scalar("SELECT status FROM control.ingestion_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .expect("fetch");
    assert_eq!(
        status, "failed",
        "orphaned run must be marked failed when configured broker is unreachable"
    );
}

/// The background loop spawned by `spawn_reconciler_loop` must heal a stuck
/// queued run within its first tick (which fires immediately on interval
/// creation, before any 2-minute delay).
///
/// In the no-producer test environment, "healed" means the run transitions to
/// `failed`.  The test polls with a 5 s wall-clock timeout so it fails fast
/// rather than hanging indefinitely on a broken CI environment.
#[tokio::test]
async fn reconciler_loop_heals_stuck_queued_run_on_first_tick() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let (tenant_id, user_id, repo_id) = insert_fixtures(&pool).await;
    // Insert a run that is 10 minutes old — well past the 2-minute threshold.
    let run_id = insert_queued_run(&pool, tenant_id, repo_id, user_id, true).await;

    let state = make_state(pool.clone());
    let handle = spawn_reconciler_loop(state);

    // Poll until the run transitions out of `queued`, with a 5 s timeout.
    // The first tick of the interval fires immediately, so this should
    // complete in milliseconds under normal conditions.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let status: String =
            sqlx::query_scalar("SELECT status FROM control.ingestion_runs WHERE id = $1")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .expect("fetch status");
        if status != "queued" {
            break;
        }
        if std::time::Instant::now() >= deadline {
            handle.abort();
            panic!("reconciler loop did not heal the stuck run within 5 s");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    handle.abort();

    let final_status: String =
        sqlx::query_scalar("SELECT status FROM control.ingestion_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .expect("fetch final status");
    assert_eq!(
        final_status, "failed",
        "stuck-queued run must be marked failed by the background reconciler (no producer in test)"
    );
}
