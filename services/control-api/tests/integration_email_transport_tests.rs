//! Signup → login matrix parameterized over every email transport.
//!
//! Regression guard for RUSAA-1592: the `console` transport auto-verify
//! path was never exercised in CI, allowing a silent breakage to exist
//! for 36 minutes before detection.
//!
//! Transport semantics under test:
//!   noop    — silent discard; signup auto-verifies (`email_verification_required=false`)
//!   console — prints a banner to stdout; signup auto-verifies
//!   smtp    — production delivery; signup does NOT auto-verify
//!
//! The actual email sender is always `noop` in these tests so CI does not
//! require a live SMTP relay or a mailpit container. Only
//! `config.email_transport` drives the auto-verify decision in the signup
//! handler.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use rb_auth::{LoginRateLimiter, PasswordHasher};
use rb_email::from_transport;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt as _;
use uuid::Uuid;

use control_api::{AppState, Config, SessionCreateRateLimiter, TenantSessionCount, build_public};
use rb_sse::{EventBus, SseConfig};

fn smtp_cfg() -> rb_email::SmtpConfig {
    rb_email::SmtpConfig {
        host: "localhost".to_string(),
        port: 587,
        username: String::new(),
        password: String::new(),
        from_address: "test@example.com".to_owned(),
    }
}

/// Build an `AppState` whose `config.email_transport` is set to
/// `transport_name`. The actual sender is always `noop` so no live relay
/// is needed.
async fn state_with_transport(transport_name: &str) -> Option<(AppState, PgPool)> {
    let db_url = std::env::var("RB_DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .ok()?;
    let email_sender = from_transport("noop", &smtp_cfg()).ok()?;
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
        email_transport: transport_name.to_owned(),
        service_name: "control-api-test".to_owned(),
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
        hybrid_search_enabled: false,
        multi_query_n: 1,
    };
    let state = AppState {
        pool: pool.clone(),
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
        kafka_consistency: Arc::new(control_api::KafkaConsistencyState::new()),
        mcp_sessions: control_api::McpSessionStore::new(),
        agent_registry: control_api::AgentRegistry::new(),
        agent_commands_producer: None,
        internal_secret: "test-internal-secret".to_owned(),
        session_create_rate_limiter: Arc::new(SessionCreateRateLimiter::default()),
        tenant_session_count: Arc::new(TenantSessionCount::new()),
        mcp_jwt_secret: "test-mcp-jwt-secret".to_owned(),
        mcp_jwt_ttl_secs: 900,
        llm_api_key: String::new(),
    };
    Some((state, pool))
}

fn json_body(v: &serde_json::Value) -> Body {
    Body::from(serde_json::to_vec(v).expect("serialise JSON"))
}

/// Shared assertion core: signup → (DB-verify if smtp) → login.
///
/// Asserts:
/// - Signup returns 201.
/// - `email_verification_required` on signup response is `false` for
///   noop/console (auto-verified) and `true` for smtp.
/// - After verification, login returns 200 with
///   `email_verification_required: false`.
async fn assert_signup_login_completes(transport: &str) {
    let Some((state, pool)) = state_with_transport(transport).await else {
        return;
    };
    let app = build_public(state);
    let email = format!(
        "integ-transport-{}-{}@test.example",
        transport,
        Uuid::new_v4().simple()
    );
    let password = "correct-horse-battery-staple";

    // --- signup ---
    let signup_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/signup")
                .header("content-type", "application/json")
                .body(json_body(&serde_json::json!({
                    "email": email,
                    "password": password,
                    "tenant_name": format!("Transport {transport} Tenant"),
                })))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        signup_resp.status(),
        StatusCode::CREATED,
        "[{transport}] signup must return 201"
    );

    let body_bytes = axum::body::to_bytes(signup_resp.into_body(), usize::MAX)
        .await
        .expect("read signup body");
    let signup_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    // noop and console auto-verify at signup; smtp requires an email click.
    let auto_verified = matches!(transport, "console" | "noop");
    assert_eq!(
        signup_body["email_verification_required"], !auto_verified,
        "[{transport}] email_verification_required on signup"
    );

    // Verify the DB row reflects the expected state.
    let db_verified: bool = sqlx::query_scalar(
        "SELECT (email_verified_at IS NOT NULL) FROM control.users WHERE email = $1",
    )
    .bind(&email)
    .fetch_one(&pool)
    .await
    .expect("user row must exist");
    assert_eq!(
        db_verified, auto_verified,
        "[{transport}] email_verified_at in DB must match auto-verify expectation"
    );

    // smtp: simulate the user clicking the verification link.
    if !auto_verified {
        sqlx::query("UPDATE control.users SET email_verified_at = NOW() WHERE email = $1")
            .bind(&email)
            .execute(&pool)
            .await
            .expect("[smtp] email verification patch must succeed");
    }

    // --- login ---
    let login_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/login")
                .header("content-type", "application/json")
                .body(json_body(&serde_json::json!({
                    "email": email,
                    "password": password,
                })))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        login_resp.status(),
        StatusCode::OK,
        "[{transport}] login must return 200"
    );

    // Extract cookie header before consuming the response body.
    let cookie = login_resp
        .headers()
        .get("set-cookie")
        .expect("Set-Cookie must be present on login")
        .to_str()
        .unwrap()
        .to_owned();

    let body_bytes = axum::body::to_bytes(login_resp.into_body(), usize::MAX)
        .await
        .expect("read login body");
    let login_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(
        login_body["email_verification_required"], false,
        "[{transport}] login must not require verification after email is confirmed"
    );
    assert!(
        cookie.contains("rb_session="),
        "[{transport}] login cookie must contain rb_session"
    );
}

/// noop transport: emails silently discarded; signup auto-verifies.
#[tokio::test]
async fn email_transport_noop_signup_login_completes() {
    assert_signup_login_completes("noop").await;
}

/// console transport: emails printed to stdout; signup auto-verifies.
///
/// Regression guard for RUSAA-1592: this path was never exercised in CI
/// before Wave 7.
#[tokio::test]
async fn email_transport_console_signup_login_completes() {
    assert_signup_login_completes("console").await;
}

/// smtp transport: production delivery path; signup does NOT auto-verify.
///
/// The sender in this test is noop (no live SMTP relay required). The test
/// verifies that `email_verification_required=true` is returned on signup,
/// then simulates the user clicking the link via a direct DB patch before
/// asserting login succeeds.
#[tokio::test]
async fn email_transport_smtp_signup_requires_verification_then_login_completes() {
    assert_signup_login_completes("smtp").await;
}
