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

use control_api::{AppState, Config, build};

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
        neo4j_uri: None,
        neo4j_user: "neo4j".to_owned(),
        neo4j_password: None,
        kafka_bootstrap_servers: "127.0.0.1:19999".to_owned(),
        dev_test_routes: false,
        migrations_root: std::env::var("RB_MIGRATIONS_ROOT").ok().map(std::path::PathBuf::from),
        qdrant_url: None,
        ollama_url: None,
        embedding_model: "nomic-embed-text".to_owned(),
    };
    let state = AppState {
        pool: pool.clone(),
        email_sender: Arc::from(email_sender),
        hasher: Arc::new(hasher),
        login_rate_limiter: Arc::new(LoginRateLimiter::new()),
        config: Arc::new(config),
        gh: None,
        sse_bus: Arc::new(EventBus::new(SseConfig::default())),
        ingest_producer: None,
        tombstone_producer: None,
        module_tree_cache: rb_query::new_module_tree_cache(),
        graph: None,
        qdrant: None,
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
    let github_repo_id =
        i64::from_ne_bytes(repo_id.as_bytes()[0..8].try_into().expect("8 bytes"));

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

    IngestFixtures { session_token, repo_id, tenant_id }
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

    let IngestFixtures { session_token, repo_id, .. } = insert_ingest_fixtures(&pool).await;

    let resp = build(state)
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

    let IngestFixtures { session_token, repo_id, tenant_id } =
        insert_ingest_fixtures(&pool).await;

    let resp = build(state)
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
