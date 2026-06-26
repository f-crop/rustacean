//! Fast integration tests for admin v1 middleware (no real DB required).
//!
//! These tests exercise the auth middleware layer without a live database;
//! the pool is constructed lazily and never actually connects. DB-backed tests
//! for §S1.6 audit invariants live in `integration_admin_v1_audit_tests.rs`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use control_api::{
    AppState, Config, KafkaConsistencyState, McpSessionStore, SessionCreateRateLimiter,
    TenantSessionCount, build_public,
};
use http_body_util::BodyExt as _;
use rb_auth::{LoginRateLimiter, PasswordHasher};
use rb_email::from_transport;
use rb_sse::{EventBus, SseConfig};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt as _;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const ADMIN_TOKEN: &str = "admin-integration-test-token-secure-32x";

// ---------------------------------------------------------------------------
// State builders
// ---------------------------------------------------------------------------

fn lazy_config_with_token(db_url: &str) -> Config {
    Config {
        listen_addr: "127.0.0.1:0".to_owned(),
        database_url: db_url.to_owned(),
        cors_origins: vec![],
        base_url: "http://localhost".to_owned(),
        session_ttl_days: 30,
        argon2_memory_kb: 64,
        argon2_time_cost: 1,
        argon2_parallelism: 1,
        email_transport: "noop".to_owned(),
        service_name: "admin-test".to_owned(),
        secure_cookies: false,
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
        internal_secret: Some("internal-test".to_owned()),
        internal_listen_addr: "127.0.0.1:0".to_owned(),
        session_create_rate_limit: 10,
        session_create_window_secs: 60,
        tenant_session_cap: 100,
        admin_token: Some(ADMIN_TOKEN.to_owned()),
        tempo_base_url: "http://localhost:3000".to_owned(),
        chat_panel_enabled: false,
        mcp_jwt_secret: Some("test-mcp-jwt-secret".to_owned()),
        mcp_jwt_ttl_secs: 900,
        llm_api_key: None,
        hybrid_search_enabled: false,
        multi_query_n: 1,
        rerank_enabled: false,
        rerank_model_dir: std::path::PathBuf::from("/models/rerank"),
        rerank_candidate_cap: 50,
        llm_token_ceiling_per_tenant: 0,
    }
}

fn build_state_from_pool(pool: PgPool, config: Config) -> AppState {
    let smtp = rb_email::SmtpConfig {
        host: String::new(),
        port: 587,
        username: String::new(),
        password: String::new(),
        from_address: "test@example.com".to_owned(),
    };
    let email_sender = from_transport("noop", &smtp).expect("noop transport");
    let hasher = PasswordHasher::from_config(64, 1, 1).expect("hasher");
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
        agent_registry: control_api::AgentRegistry::new(),
        agent_commands_producer: None,
        internal_secret: "internal-test".to_owned(),
        session_create_rate_limiter: Arc::new(SessionCreateRateLimiter::default()),
        tenant_session_count: Arc::new(TenantSessionCount::new()),
        mcp_jwt_secret: "test-mcp-jwt-secret".to_owned(),
        mcp_jwt_ttl_secs: 900,
        llm_api_key: String::new(),
        reranker: None,
    }
}

async fn collect_body(body: Body) -> serde_json::Value {
    let bytes = body.collect().await.expect("collect body").to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

// ---------------------------------------------------------------------------
// Fast middleware tests (no real DB required — lazy pool never connects)
// ---------------------------------------------------------------------------

fn lazy_app() -> axum::Router {
    let config = lazy_config_with_token("postgres://localhost/nonexistent_test");
    let pool = PgPoolOptions::new()
        .connect_lazy(&config.database_url)
        .expect("lazy connect");
    let state = build_state_from_pool(pool, config);
    build_public(state)
}

#[tokio::test]
async fn middleware_no_auth_header_returns_401() {
    let resp = lazy_app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/v1/bootstrap/admin")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"email":"a@b.com","password":"supersecret12"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // No body on 401 — invariant §S1 (never echo the token).
    let body = collect_body(resp.into_body()).await;
    assert!(
        body.is_null() || body.as_object().is_none_or(serde_json::Map::is_empty),
        "401 must return no body, got: {body}"
    );
}

#[tokio::test]
async fn middleware_wrong_token_returns_401() {
    let resp = lazy_app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/v1/bootstrap/admin")
                .header("authorization", "Bearer definitely-wrong-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"email":"a@b.com","password":"supersecret12"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn middleware_correct_token_no_actor_returns_400() {
    // The write to auth.admin_audit_log will fail (no real DB), but the 400
    // response must still be returned — write_audit_denial is fire-and-forget.
    let resp = lazy_app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/v1/bootstrap/admin")
                .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"email":"a@b.com","password":"supersecret12"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn middleware_empty_actor_header_returns_400() {
    let resp = lazy_app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/v1/bootstrap/admin")
                .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
                .header("x-admin-actor", "   ")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"email":"a@b.com","password":"supersecret12"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
