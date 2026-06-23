//! Integration tests — chat gateway tenant isolation (ADR-013 §3, RUSAA-1813).
//!
//! Acceptance criteria verified:
//!   AC1: Session created for tenant A is invisible to tenant B (GET → 404).
//!   AC2: Message appended for tenant A is invisible to tenant B (GET messages → 404).
//!   AC3: DELETE of tenant A's session by tenant B is rejected (404).
//!   AC4: Feature-flag gate: all chat routes return 404 when `chat_panel_enabled = false`.
//!
//! Tests require a running Postgres instance at `RB_DATABASE_URL`; they skip
//! gracefully when that variable is absent.

use std::sync::Arc;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use rb_auth::{LoginRateLimiter, PasswordHasher, sha256_hex};
use rb_email::from_transport;
use rb_sse::{EventBus, SseConfig};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt as _;
use uuid::Uuid;

use control_api::{AppState, Config, SessionCreateRateLimiter, TenantSessionCount, build_public};

// ---------------------------------------------------------------------------
// State builders
// ---------------------------------------------------------------------------

async fn real_db_state_chat_enabled() -> Option<(AppState, PgPool)> {
    build_state(true).await
}

async fn real_db_state_chat_disabled() -> Option<(AppState, PgPool)> {
    build_state(false).await
}

async fn build_state(chat_panel_enabled: bool) -> Option<(AppState, PgPool)> {
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
        service_name: "control-api-chat-test".to_owned(),
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
        internal_secret: Some("test-chat-internal-secret".to_owned()),
        internal_listen_addr: "127.0.0.1:0".to_owned(),
        session_create_rate_limit: 10,
        session_create_window_secs: 60,
        tenant_session_cap: 100,
        admin_token: None,
        chat_panel_enabled,
        tempo_base_url: "http://localhost:3000".to_owned(),
        mcp_jwt_secret: Some("test-mcp-jwt-secret-chat".to_owned()),
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
        internal_secret: "test-chat-internal-secret".to_owned(),
        session_create_rate_limiter: Arc::new(SessionCreateRateLimiter::default()),
        tenant_session_count: Arc::new(TenantSessionCount::new()),
        mcp_jwt_secret: "test-mcp-jwt-secret-chat".to_owned(),
        mcp_jwt_ttl_secs: 900,
        llm_api_key: String::new(),
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

/// Seed a tenant + user + verified control session.
async fn seed_tenant_with_session(pool: &PgPool) -> TenantFixture {
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let ctrl_session_id = Uuid::new_v4();

    let slug = format!("chat-test-{}", tenant_id.simple());
    let schema_name = format!("chat_{}", tenant_id.simple());

    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(&slug)
    .bind("Chat Test Tenant")
    .bind(&schema_name)
    .execute(pool)
    .await
    .expect("insert tenant");

    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, email_verified_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(user_id)
    .bind(format!("chat-{}@test.example", user_id.simple()))
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

    let session_token = format!("chat-test-token-{}", Uuid::new_v4().simple());
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

/// Insert a `chat_session` row directly for a given tenant (bypasses Kafka).
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
// AC1 — GET session: tenant A can read its own session; tenant B cannot
// ---------------------------------------------------------------------------

/// Tenant A reads its own chat session — must return 200.
#[tokio::test]
async fn ac1a_get_own_chat_session_returns_200() {
    let Some((state, pool)) = real_db_state_chat_enabled().await else {
        return;
    };
    let a = seed_tenant_with_session(&pool).await;
    let session_id = seed_chat_session(&pool, a.tenant_id, a.user_id).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1/chat/sessions/{session_id}"))
                .header("cookie", format!("rb_session={}", a.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "tenant A must be able to read its own chat session"
    );
}

/// Tenant B attempts to read tenant A's chat session — must return 404.
///
/// Tenant isolation is enforced at the DB query level: `db_get_chat_session`
/// filters by `(id, tenant_id)` so a session belonging to a different tenant
/// is indistinguishable from a non-existent session.
#[tokio::test]
async fn ac1b_cross_tenant_get_returns_404() {
    let Some((state, pool)) = real_db_state_chat_enabled().await else {
        return;
    };
    let a = seed_tenant_with_session(&pool).await;
    let b = seed_tenant_with_session(&pool).await;
    let session_id = seed_chat_session(&pool, a.tenant_id, a.user_id).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1/chat/sessions/{session_id}"))
                .header("cookie", format!("rb_session={}", b.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "tenant B must not see tenant A's chat session"
    );
}

// ---------------------------------------------------------------------------
// AC2 — GET messages: tenant B cannot list messages for tenant A's session
// ---------------------------------------------------------------------------

/// Tenant B attempts to list messages for tenant A's chat session — must return 404.
#[tokio::test]
async fn ac2_cross_tenant_list_messages_returns_404() {
    let Some((state, pool)) = real_db_state_chat_enabled().await else {
        return;
    };
    let a = seed_tenant_with_session(&pool).await;
    let b = seed_tenant_with_session(&pool).await;
    let session_id = seed_chat_session(&pool, a.tenant_id, a.user_id).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1/chat/sessions/{session_id}/messages"))
                .header("cookie", format!("rb_session={}", b.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "tenant B must not list messages for tenant A's chat session"
    );
}

// ---------------------------------------------------------------------------
// AC3 — DELETE session: tenant B cannot terminate tenant A's session
// ---------------------------------------------------------------------------

/// Tenant B attempts to DELETE tenant A's chat session — must return 404.
#[tokio::test]
async fn ac3_cross_tenant_delete_returns_404() {
    let Some((state, pool)) = real_db_state_chat_enabled().await else {
        return;
    };
    let a = seed_tenant_with_session(&pool).await;
    let b = seed_tenant_with_session(&pool).await;
    let session_id = seed_chat_session(&pool, a.tenant_id, a.user_id).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/v1/chat/sessions/{session_id}"))
                .header("cookie", format!("rb_session={}", b.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "tenant B must not be able to delete tenant A's chat session"
    );
}

/// Tenant A can delete its own session — must return 202.
/// Kafka is not configured in tests, so no terminate command is published;
/// the DB is updated synchronously.
#[tokio::test]
async fn ac3b_own_delete_returns_202() {
    let Some((state, pool)) = real_db_state_chat_enabled().await else {
        return;
    };
    let a = seed_tenant_with_session(&pool).await;
    let session_id = seed_chat_session(&pool, a.tenant_id, a.user_id).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/v1/chat/sessions/{session_id}"))
                .header("cookie", format!("rb_session={}", a.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::ACCEPTED,
        "deleting own active session must return 202"
    );

    // Verify DB was updated.
    let (status,): (String,) =
        sqlx::query_as("SELECT status FROM control.chat_sessions WHERE id = $1")
            .bind(session_id)
            .fetch_one(&pool)
            .await
            .expect("fetch session status");
    assert_eq!(
        status, "ended",
        "session status must be 'ended' after DELETE"
    );
}

// ---------------------------------------------------------------------------
// AC4 — Feature flag gate: all chat routes return 404 when disabled
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// AC5 — GET /v1/chat/sessions list endpoint
// ---------------------------------------------------------------------------

/// GET /v1/chat/sessions returns 200 with empty list when no sessions exist.
#[tokio::test]
async fn ac5a_list_chat_sessions_empty_returns_200() {
    let Some((state, pool)) = real_db_state_chat_enabled().await else {
        return;
    };
    let a = seed_tenant_with_session(&pool).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/chat/sessions")
                .header("cookie", format!("rb_session={}", a.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "GET /v1/chat/sessions must return 200 for authenticated user with no sessions"
    );

    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json["sessions"],
        serde_json::json!([]),
        "response must contain empty sessions array"
    );
}

/// GET /v1/chat/sessions returns only the calling user's sessions, not another tenant's.
#[tokio::test]
async fn ac5b_list_chat_sessions_tenant_isolation() {
    let Some((state, pool)) = real_db_state_chat_enabled().await else {
        return;
    };
    let a = seed_tenant_with_session(&pool).await;
    let b = seed_tenant_with_session(&pool).await;
    let _session_a = seed_chat_session(&pool, a.tenant_id, a.user_id).await;

    // Tenant B lists — must see zero sessions (not tenant A's)
    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/chat/sessions")
                .header("cookie", format!("rb_session={}", b.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json["sessions"].as_array().unwrap().len(),
        0,
        "tenant B must not see tenant A's sessions in list"
    );
}

/// GET /v1/chat/sessions returns the user's own session after creation.
#[tokio::test]
async fn ac5c_list_chat_sessions_returns_own_session() {
    let Some((state, pool)) = real_db_state_chat_enabled().await else {
        return;
    };
    let a = seed_tenant_with_session(&pool).await;
    let session_id = seed_chat_session(&pool, a.tenant_id, a.user_id).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/chat/sessions")
                .header("cookie", format!("rb_session={}", a.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let sessions = json["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "must return exactly one session");
    assert_eq!(
        sessions[0]["id"].as_str().unwrap(),
        session_id.to_string(),
        "returned session id must match seeded session"
    );
}

/// With `chat_panel_enabled = false`, GET /v1/chat/sessions returns 404.
#[tokio::test]
async fn ac5d_list_chat_sessions_disabled_returns_404() {
    let Some((state, pool)) = real_db_state_chat_disabled().await else {
        return;
    };
    let a = seed_tenant_with_session(&pool).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/chat/sessions")
                .header("cookie", format!("rb_session={}", a.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "GET /v1/chat/sessions must return 404 when feature flag is off"
    );
}

/// With `chat_panel_enabled = false`, GET /v1/chat/sessions/{id} returns 404.
#[tokio::test]
async fn ac4_chat_feature_disabled_get_returns_404() {
    let Some((state, pool)) = real_db_state_chat_disabled().await else {
        return;
    };
    let a = seed_tenant_with_session(&pool).await;
    let session_id = seed_chat_session(&pool, a.tenant_id, a.user_id).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1/chat/sessions/{session_id}"))
                .header("cookie", format!("rb_session={}", a.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "chat routes must return 404 when feature flag is off"
    );
}
