//! Regression-defence test for RUSAA-1884: multi-turn chat sessions.
//!
//! Verifies that `control.chat_sessions.status` stays `'active'` across
//! multiple POST /v1/chat/sessions/{id}/messages calls so the second (and
//! subsequent) messages are not rejected with 422 `ChatSessionNotActive`.
//!
//! Tests skip gracefully when `RB_DATABASE_URL` is absent.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use control_api::{AppState, Config, SessionCreateRateLimiter, TenantSessionCount, build_public};
use rb_auth::{LoginRateLimiter, PasswordHasher, sha256_hex};
use rb_email::from_transport;
use rb_sse::{EventBus, SseConfig};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt as _;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// State builder
// ---------------------------------------------------------------------------

async fn real_db_state_chat_enabled() -> Option<(AppState, PgPool)> {
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
        service_name: "control-api-rusaa1884-test".to_owned(),
        secure_cookies: true,
        gh_app_id: None,
        gh_app_private_key_b64: None,
        gh_app_webhook_secret: None,
        gh_app_enc_key_b64: None,
        gh_api_base: rb_github::DEFAULT_GITHUB_API_BASE.to_owned(),
        neo4j_uri: None,
        neo4j_user: "neo4j".to_owned(),
        neo4j_password: None,
        kafka_bootstrap_servers: "localhost:9092".to_owned(),
        dev_test_routes: false,
        migrations_root: None,
        qdrant_url: None,
        ollama_url: None,
        embedding_model: "nomic-embed-text".to_owned(),
        internal_secret: Some("test-1884-internal-secret".to_owned()),
        internal_listen_addr: "127.0.0.1:0".to_owned(),
        session_create_rate_limit: 10,
        session_create_window_secs: 60,
        tenant_session_cap: 100,
        admin_token: None,
        chat_panel_enabled: true,
        tempo_base_url: "http://localhost:3000".to_owned(),
        mcp_jwt_secret: Some("test-1884-mcp-jwt-secret".to_owned()),
        mcp_jwt_ttl_secs: 900,
        llm_api_key: None,
        hybrid_search_enabled: false,
        multi_query_n: 1,
        rerank_enabled: false,
        rerank_model_dir: std::path::PathBuf::from("/models/rerank"),
        rerank_candidate_cap: 50,
        llm_token_ceiling_per_tenant: 0,
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
        internal_secret: "test-1884-internal-secret".to_owned(),
        session_create_rate_limiter: Arc::new(SessionCreateRateLimiter::default()),
        tenant_session_count: Arc::new(TenantSessionCount::new()),
        mcp_jwt_secret: "test-1884-mcp-jwt-secret".to_owned(),
        mcp_jwt_ttl_secs: 900,
        llm_api_key: String::new(),
        reranker: None,
        llm_tenant_tokens: std::sync::Arc::new(control_api::TenantLlmTokenCounter::new()),
    };
    Some((state, pool))
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

struct TenantFixture {
    tenant_id: Uuid,
    user_id: Uuid,
    session_token: String,
}

async fn seed_tenant_with_session(pool: &PgPool) -> TenantFixture {
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let ctrl_session_id = Uuid::new_v4();

    let slug = format!("1884-test-{}", tenant_id.simple());
    let schema_name = format!("r1884_{}", tenant_id.simple());

    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(&slug)
    .bind("RUSAA-1884 Test Tenant")
    .bind(&schema_name)
    .execute(pool)
    .await
    .expect("insert tenant");

    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, email_verified_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(user_id)
    .bind(format!("1884-{}@test.example", user_id.simple()))
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

    let session_token = format!("1884-test-token-{}", Uuid::new_v4().simple());
    let token_hash = sha256_hex(&session_token);
    sqlx::query(
        "INSERT INTO control.sessions (id, user_id, tenant_id, token_hash, expires_at) \
         VALUES ($1, $2, $3, $4, now() + interval '30 days')",
    )
    .bind(ctrl_session_id)
    .bind(user_id)
    .bind(tenant_id)
    .bind(&token_hash)
    .execute(pool)
    .await
    .expect("insert control session");

    TenantFixture {
        tenant_id,
        user_id,
        session_token,
    }
}

async fn seed_chat_session(pool: &PgPool, tenant_id: Uuid, user_id: Uuid) -> Uuid {
    let session_id = Uuid::new_v4();
    let trace_id = format!("{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO control.chat_sessions \
         (id, tenant_id, user_id, runtime, trace_id) \
         VALUES ($1, $2, $3, 'claude_code', $4)",
    )
    .bind(session_id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(&trace_id)
    .execute(pool)
    .await
    .expect("insert chat_session");
    session_id
}

// ---------------------------------------------------------------------------
// RUSAA-1884: multi-turn chat — session stays 'active' across messages
// ---------------------------------------------------------------------------

/// Posting multiple messages to an active session must not flip
/// `chat_sessions.status` away from 'active'.  The status must remain
/// 'active' after each message so the next POST returns 202 (not 422).
///
/// Kafka is not configured in this test, so the route returns 503 (producer
/// absent) after the status check.  We validate that the status check
/// itself passes (i.e., the session is still 'active') rather than
/// returning 422 `ChatSessionNotActive`.
#[tokio::test]
async fn rusaa1884_session_stays_active_across_messages() {
    let Some((state, pool)) = real_db_state_chat_enabled().await else {
        return;
    };
    let a = seed_tenant_with_session(&pool).await;
    let session_id = seed_chat_session(&pool, a.tenant_id, a.user_id).await;

    let app = build_public(state);

    // First message — Kafka not configured → 503, but session must still be 'active'.
    let resp1 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/chat/sessions/{session_id}/messages"))
                .header("cookie", format!("rb_session={}", a.session_token))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"content":"hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    // 503 means Kafka was unreachable — NOT 422 (session not active).
    assert_ne!(
        resp1.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "first message must not fail with ChatSessionNotActive (422)"
    );

    // Confirm session is still 'active' in the DB — the status check in
    // messages.rs passed before Kafka was reached.
    let (status,): (String,) =
        sqlx::query_as("SELECT status FROM control.chat_sessions WHERE id = $1")
            .bind(session_id)
            .fetch_one(&pool)
            .await
            .expect("fetch session status");
    assert_eq!(
        status, "active",
        "session status must remain 'active' after first message attempt"
    );

    // Second message — must also pass the status check (not 422).
    let resp2 = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/chat/sessions/{session_id}/messages"))
                .header("cookie", format!("rb_session={}", a.session_token))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"content":"follow-up"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_ne!(
        resp2.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "second message must not fail with ChatSessionNotActive (422)"
    );
}
