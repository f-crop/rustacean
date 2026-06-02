//! Cookie security flag assertions — extracted from `integration_tests.rs` to keep
//! that file under the 600-line cap.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use control_api::{AppState, Config, KafkaConsistencyState, McpSessionStore, build_public};
use rb_auth::{LoginRateLimiter, PasswordHasher};
use rb_email::from_transport;
use rb_sse::{EventBus, SseConfig};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt as _;
use uuid::Uuid;

async fn real_db_app() -> Option<axum::Router> {
    let db_url = std::env::var("RB_DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(3)
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
        service_name: "cookie-test".to_owned(),
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
        internal_secret: "test-internal-secret".to_owned(),
        session_create_rate_limiter: Arc::new(control_api::SessionCreateRateLimiter::new(10, 60)),
        tenant_session_count: Arc::new(control_api::TenantSessionCount::new()),
        mcp_jwt_secret: "test-mcp-jwt-secret".to_owned(),
        mcp_jwt_ttl_secs: 900,
        llm_api_key: String::new(),
    };
    Some(build_public(state))
}

/// Signup response must carry `Secure` + `HttpOnly` on the session cookie.
#[tokio::test]
async fn integration_signup_cookie_has_secure_flag() {
    let Some(app) = real_db_app().await else {
        return;
    };
    let email = format!("cookie-sec-{}@test.example", Uuid::new_v4().simple());
    let body = serde_json::json!({
        "email": email,
        "password": "correct-horse-battery-staple",
        "tenant_name": "Secure Cookie Test Tenant",
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/signup")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED, "signup must return 201");
    let cookie = resp
        .headers()
        .get("set-cookie")
        .expect("Set-Cookie header must be present on signup")
        .to_str()
        .unwrap();
    assert!(
        cookie.contains("rb_session="),
        "cookie must contain rb_session token"
    );
    assert!(
        cookie.contains("Secure"),
        "signup cookie must carry the Secure flag"
    );
    assert!(
        cookie.contains("HttpOnly"),
        "signup cookie must carry HttpOnly"
    );
}
