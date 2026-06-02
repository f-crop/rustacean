//! Integration tests for `POST /v1/repos/{repo_id}/ingestions` (REQ-IN-01).
//!
//! These tests require a running Postgres instance accessible via
//! `RB_DATABASE_URL`. When that variable is absent the tests skip gracefully.
//!
//! AC5: broker unreachable → 503 `kafka_unavailable`
//! AC6: Kafka publish failure → DB transaction rolled back (no orphan rows)

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt as _;
use rb_auth::{LoginRateLimiter, PasswordHasher, sha256_hex};
use rb_email::from_transport;
use rb_kafka::ProducerCfg;
use rb_schemas::IngestRequest;
use rb_sse::{EventBus, SseConfig};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt as _;
use uuid::Uuid;

use control_api::{AppState, Config, SessionCreateRateLimiter, TenantSessionCount, build_public};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Build an `AppState` connected to a real Postgres instance.
/// Returns `None` when `RB_DATABASE_URL` is absent — callers skip gracefully.
async fn real_db_state() -> Option<(AppState, PgPool)> {
    let db_url = std::env::var("RB_DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .ok()?;
    let smtp = rb_email::SmtpConfig {
        host: String::new(),
        port: 587,
        username: String::new(),
        password: String::new(),
        from_address: "test@example.com".to_owned(),
    };
    let email_sender = from_transport("noop", &smtp).ok()?;
    let hasher = PasswordHasher::from_config(64, 1, 1).ok()?;
    let config = Config {
        listen_addr: "127.0.0.1:0".to_owned(),
        database_url: db_url,
        cors_origins: vec![],
        base_url: "http://localhost:8080".to_owned(),
        session_ttl_days: 30,
        argon2_memory_kb: 64,
        argon2_time_cost: 1,
        argon2_parallelism: 1,
        email_transport: "noop".to_owned(),
        service_name: "control-api-ingest-test".to_owned(),
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
        migrations_root: std::env::var("RB_MIGRATIONS_ROOT")
            .ok()
            .map(std::path::PathBuf::from),
        qdrant_url: None,
        ollama_url: None,
        embedding_model: "nomic-embed-text".to_owned(),
        internal_secret: Some("test-internal-secret".to_owned()),
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
    let state = AppState {
        pool: pool.clone(),
        email_sender: Arc::from(email_sender),
        hasher: Arc::new(hasher),
        login_rate_limiter: Arc::new(LoginRateLimiter::new()),
        config: Arc::new(config),
        gh_loader: std::sync::Arc::new(rb_github::GhAppLoader::new(None)),
        sse_bus: Arc::new(EventBus::new(SseConfig::default())),
        ingest_producer: None,
        tombstone_producer: None,
        module_tree_cache: rb_query::new_module_tree_cache(),
        graph: None,
        qdrant: None,
        http_client: reqwest::Client::new(),
        neo4j_uri: None,
        kafka_consistency: Arc::new(control_api::KafkaConsistencyState::new()),
        mcp_sessions: control_api::McpSessionStore::new(),
        agent_registry: control_api::AgentRegistry::new(),
        agent_commands_producer: None,
        internal_secret: "test-internal-secret".to_owned(),
        session_create_rate_limiter: Arc::new(SessionCreateRateLimiter::default()),
        tenant_session_count: Arc::new(TenantSessionCount::new()),
            mcp_jwt_secret: "test-mcp-jwt-secret".to_owned(),
            mcp_jwt_ttl_secs: 900,
    };
    Some((state, pool))
}

/// Fixture result: everything the caller needs to drive the trigger endpoint.
struct IngestFixtures {
    session_token: String,
    repo_id: Uuid,
    tenant_id: Uuid,
}

/// Insert the minimal set of control-schema rows required to reach the Kafka
/// publish step in `POST /v1/repos/{id}/ingestions`:
/// tenant → user (email-verified) → session → `github_installation` → repo.
///
/// All rows use fresh UUIDs so parallel test runs never collide.
async fn insert_ingest_fixtures(pool: &PgPool) -> IngestFixtures {
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let install_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();

    let slug = format!("ingest-test-{}", tenant_id.simple());
    let schema_name = format!("ingest_{}", tenant_id.simple());

    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(&slug)
    .bind("Ingest Integration Tenant")
    .bind(&schema_name)
    .execute(pool)
    .await
    .expect("insert tenant");

    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, email_verified_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(user_id)
    .bind(format!("ingest-{}@test.example", user_id.simple()))
    .bind("$argon2id$v=19$m=65536,t=1,p=1$placeholder_hash")
    .execute(pool)
    .await
    .expect("insert user");

    sqlx::query(
        "INSERT INTO control.tenant_members (tenant_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(tenant_id)
    .bind(user_id)
    .execute(pool)
    .await
    .expect("insert tenant_member");

    let session_token = format!("ingest-test-token-{}", Uuid::new_v4().simple());
    let token_hash = sha256_hex(&session_token);
    sqlx::query(
        "INSERT INTO control.sessions (id, user_id, tenant_id, token_hash, expires_at) \
         VALUES ($1, $2, $3, $4, now() + interval '30 days')",
    )
    .bind(session_id)
    .bind(user_id)
    .bind(tenant_id)
    .bind(&token_hash)
    .execute(pool)
    .await
    .expect("insert session");

    // Derive unique i64 values for Postgres BIGINT columns from UUID bytes.
    // from_ne_bytes reinterprets 8 bytes as i64; no truncation occurs.
    let github_install_id =
        i64::from_ne_bytes(install_id.as_bytes()[0..8].try_into().expect("8 bytes"));
    let github_repo_id = i64::from_ne_bytes(repo_id.as_bytes()[0..8].try_into().expect("8 bytes"));

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

    IngestFixtures {
        session_token,
        repo_id,
        tenant_id,
    }
}

/// A `ProducerCfg` that points at an unreachable local port with a short
/// delivery timeout so tests complete in well under one second.
fn unreachable_producer_cfg() -> ProducerCfg {
    ProducerCfg {
        bootstrap_servers: "127.0.0.1:19999".to_owned(),
        compression_type: "none".to_owned(),
        linger_ms: 0,
        delivery_timeout_ms: 500,
        queue_buffering_max_kbytes: 1024,
    }
}

// ---------------------------------------------------------------------------
// AC5 — 503 when broker unreachable
// ---------------------------------------------------------------------------

/// AC5: `POST /v1/repos/{id}/ingestions` must return **503 `kafka_unavailable`**
/// when the Kafka broker is unreachable, not 500 `internal_error`.
///
/// The producer is configured with `127.0.0.1:19999` (nothing listening) and a
/// 500 ms delivery timeout — librdkafka fails fast with `AllBrokersDown` or an
/// equivalent timeout code, which `KafkaError::is_broker_unavailable()` maps to
/// HTTP 503.
#[tokio::test]
async fn ac5_trigger_returns_503_when_broker_unreachable() {
    let Some((mut state, pool)) = real_db_state().await else {
        return; // skip: no DB
    };

    let producer = rb_kafka::Producer::<IngestRequest>::new(&unreachable_producer_cfg())
        .expect("producer construction succeeds even with unreachable bootstrap");
    state.ingest_producer = Some(Arc::new(producer));

    let IngestFixtures {
        session_token,
        repo_id,
        ..
    } = insert_ingest_fixtures(&pool).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/repos/{repo_id}/ingestions"))
                .header("content-type", "application/json")
                .header("cookie", format!("rb_session={session_token}"))
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "AC5: unreachable broker must yield 503, got {}",
        resp.status()
    );

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json["error"], "kafka_unavailable",
        "AC5: error code must be 'kafka_unavailable', got {json:?}"
    );
}

// ---------------------------------------------------------------------------
// AC6 — DB rollback on Kafka publish failure
// ---------------------------------------------------------------------------

/// AC6: After a Kafka publish failure, neither `ingestion_runs` nor
/// `pipeline_stage_runs` rows must persist (transaction rolled back).
///
/// Same unreachable-broker setup as AC5; after the 503 response we query
/// Postgres directly to confirm zero rows exist for the test repo.
#[tokio::test]
async fn ac6_trigger_rolls_back_db_on_kafka_failure() {
    let Some((mut state, pool)) = real_db_state().await else {
        return; // skip: no DB
    };

    let producer = rb_kafka::Producer::<IngestRequest>::new(&unreachable_producer_cfg())
        .expect("producer construction succeeds even with unreachable bootstrap");
    state.ingest_producer = Some(Arc::new(producer));

    let IngestFixtures {
        session_token,
        repo_id,
        tenant_id,
    } = insert_ingest_fixtures(&pool).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/repos/{repo_id}/ingestions"))
                .header("content-type", "application/json")
                .header("cookie", format!("rb_session={session_token}"))
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    // Must not be a 2xx — some kind of 5xx is expected.
    assert!(
        resp.status().is_server_error(),
        "AC6: Kafka failure must not return 2xx, got {}",
        resp.status()
    );

    // No ingestion_runs row must survive the rolled-back transaction.
    let (run_count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM control.ingestion_runs \
         WHERE repo_id = $1 AND tenant_id = $2",
    )
    .bind(repo_id)
    .bind(tenant_id)
    .fetch_one(&pool)
    .await
    .expect("count ingestion_runs");

    assert_eq!(
        run_count, 0,
        "AC6: ingestion_runs must be absent after Kafka publish failure"
    );

    // pipeline_stage_runs cascade from ingestion_runs; confirm absence too.
    let (stage_count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM control.pipeline_stage_runs psr \
         INNER JOIN control.ingestion_runs ir ON ir.id = psr.ingestion_run_id \
         WHERE ir.repo_id = $1 AND ir.tenant_id = $2",
    )
    .bind(repo_id)
    .bind(tenant_id)
    .fetch_one(&pool)
    .await
    .expect("count pipeline_stage_runs");

    assert_eq!(
        stage_count, 0,
        "AC6: pipeline_stage_runs must be absent after Kafka publish failure"
    );
}

// ---------------------------------------------------------------------------
// RUSAA-1560 — finished_at must advance to MAX(stage.finished_at)
// ---------------------------------------------------------------------------

/// AC: `ingestion_runs.finished_at ≥ MAX(pipeline_stage_runs.finished_at)` across
/// all 9 stages.
///
/// Verifies the two-UPDATE logic in `maybe_complete_run`:
/// 1. Initial completion: `finished_at` is set to MAX of stage timestamps, not `now()`.
/// 2. Subsequent fan-out Done events: `finished_at` advances as later stages complete.
///
/// This test does NOT call Kafka; it exercises the DB queries directly to confirm
/// the SQL semantics of the fix.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn rusaa_1560_finished_at_equals_max_stage_finished_at() {
    let Some((_state, pool)) = real_db_state().await else {
        return; // skip: no DB
    };

    // --- Setup minimal rows ---
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();

    let slug = format!("rusaa1560-{}", tenant_id.simple());
    let schema_name = format!("rusaa1560_{}", tenant_id.simple());

    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(&slug)
    .bind("RUSAA-1560 Test Tenant")
    .bind(&schema_name)
    .execute(&pool)
    .await
    .expect("insert tenant");

    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, email_verified_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(user_id)
    .bind(format!("rusaa1560-{}@test.example", user_id.simple()))
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
    .bind(42_i64)
    .bind("test-org/rusaa1560-repo")
    .bind("main")
    .bind(user_id)
    .execute(&pool)
    .await
    .expect("insert repo");

    sqlx::query(
        "INSERT INTO control.ingestion_runs (id, tenant_id, repo_id, status, requested_by) \
         VALUES ($1, $2, $3, 'running', $4)",
    )
    .bind(run_id)
    .bind(tenant_id)
    .bind(repo_id)
    .bind(user_id)
    .execute(&pool)
    .await
    .expect("insert ingestion_run");

    // Insert all 9 stages with succeeded status. Serial stages get early timestamps;
    // fan-out stages (extract onwards) get progressively later ones — simulating that
    // they finish long after the serial chain completes.
    let stages: &[(&str, &str)] = &[
        ("clone", "now() - interval '300 seconds'"),
        ("expand", "now() - interval '299 seconds'"),
        ("parse", "now() - interval '298 seconds'"),
        ("typecheck", "now() - interval '291 seconds'"), // serial chain done ~9s in
        ("extract", "now() - interval '33 seconds'"),    // fan-out: first item early
        ("embed", "now() - interval '27 seconds'"),
        ("project_pg", "now() - interval '17 seconds'"),
        ("project_neo4j", "now() - interval '4 seconds'"),
        ("project_qdrant", "now() - interval '2 seconds'"), // last to finish
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

    // --- Execute the two UPDATEs from maybe_complete_run ---
    sqlx::query(
        "UPDATE control.ingestion_runs \
         SET status = 'succeeded' \
         WHERE id = $1 AND status IN ('queued', 'running')",
    )
    .bind(run_id)
    .execute(&pool)
    .await
    .expect("status transition");

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

    // --- Verify AC: finished_at == MAX(stage.finished_at) ---
    let (run_finished_at, max_stage_finished_at): (
        Option<chrono::DateTime<chrono::Utc>>,
        Option<chrono::DateTime<chrono::Utc>>,
    ) = sqlx::query_as(
        "SELECT ir.finished_at, \
                (SELECT MAX(psr.finished_at) FROM control.pipeline_stage_runs psr \
                 WHERE psr.ingestion_run_id = ir.id) \
         FROM control.ingestion_runs ir \
         WHERE ir.id = $1",
    )
    .bind(run_id)
    .fetch_one(&pool)
    .await
    .expect("fetch run + max stage time");

    let run_ts = run_finished_at.expect("finished_at must not be NULL after completion");
    let max_ts = max_stage_finished_at.expect("at least one stage must have a finished_at");

    assert_eq!(
        run_ts, max_ts,
        "RUSAA-1560: ingestion_runs.finished_at must equal MAX(pipeline_stage_runs.finished_at); \
         got run={run_ts}, max_stage={max_ts}"
    );

    // --- Simulate a second fan-out Done event (later item) ---
    // Advance project_qdrant to the LATEST timestamp, representing the last item processed.
    sqlx::query(
        "UPDATE control.pipeline_stage_runs \
         SET finished_at = now() \
         WHERE ingestion_run_id = $1 AND stage = 'project_qdrant'",
    )
    .bind(run_id)
    .execute(&pool)
    .await
    .expect("advance project_qdrant finished_at");

    // Re-run the finished_at advance (as maybe_complete_run would after the next Done event).
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
    .expect("re-advance finished_at after later item");

    let (new_run_ts, new_max_ts): (
        Option<chrono::DateTime<chrono::Utc>>,
        Option<chrono::DateTime<chrono::Utc>>,
    ) = sqlx::query_as(
        "SELECT ir.finished_at, \
                (SELECT MAX(psr.finished_at) FROM control.pipeline_stage_runs psr \
                 WHERE psr.ingestion_run_id = ir.id) \
         FROM control.ingestion_runs ir \
         WHERE ir.id = $1",
    )
    .bind(run_id)
    .fetch_one(&pool)
    .await
    .expect("re-fetch run + max stage time");

    let new_run_ts = new_run_ts.expect("finished_at still non-null after second event");
    let new_max_ts = new_max_ts.expect("max stage still non-null");

    assert!(
        new_run_ts > run_ts,
        "RUSAA-1560: finished_at must advance when a later fan-out item arrives; \
         before={run_ts}, after={new_run_ts}"
    );
    assert_eq!(
        new_run_ts, new_max_ts,
        "RUSAA-1560: finished_at must equal MAX(stage.finished_at) after second advance; \
         got run={new_run_ts}, max_stage={new_max_ts}"
    );
}
