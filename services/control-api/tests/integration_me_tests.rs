use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt as _;
use rb_auth::{LoginRateLimiter, PasswordHasher};
use rb_email::from_transport;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt as _;
use uuid::Uuid;

use control_api::{AppState, Config, SessionCreateRateLimiter, TenantSessionCount, build_public};
use rb_sse::{EventBus, SseConfig};

async fn collect_body(body: Body) -> Vec<u8> {
    body.collect()
        .await
        .expect("collect body")
        .to_bytes()
        .to_vec()
}

/// Build a state connected to a real Postgres instance.
///
/// Returns `None` when `RB_DATABASE_URL` is absent so callers can skip
/// gracefully instead of panicking.
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
        llm_api_key: String::new(),
    };
    Some((state, pool))
}

fn json_body(v: &serde_json::Value) -> Body {
    Body::from(serde_json::to_vec(v).expect("serialise JSON"))
}

// ---------------------------------------------------------------------------
// REQ-AU-06: GET /v1/me — session refresh + current user/tenant view
// ---------------------------------------------------------------------------

/// Helper: signup + (optional) email verification + login, returns `rb_session` token.
async fn signup_and_login(
    app: axum::Router,
    pool: &PgPool,
    email: &str,
    password: &str,
    tenant_name: &str,
    verify_email: bool,
) -> String {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/signup")
                .header("content-type", "application/json")
                .body(json_body(&serde_json::json!({
                    "email": email,
                    "password": password,
                    "tenant_name": tenant_name,
                })))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "signup must return 201");

    if verify_email {
        sqlx::query("UPDATE control.users SET email_verified_at = NOW() WHERE email = $1")
            .bind(email)
            .execute(pool)
            .await
            .expect("email verification patch must succeed");
    }

    let resp = app
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
    assert_eq!(resp.status(), StatusCode::OK, "login must return 200");

    let cookie = resp
        .headers()
        .get("set-cookie")
        .expect("Set-Cookie header must be present")
        .to_str()
        .unwrap();
    let token_kv = cookie
        .split(';')
        .next()
        .expect("cookie must have a token segment")
        .trim();
    assert!(token_kv.starts_with("rb_session="));
    token_kv["rb_session=".len()..].to_owned()
}

#[tokio::test]
async fn integration_me_returns_user_and_tenant_for_active_session() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let app = build_public(state);
    let email = format!("integ-me-ok-{}@test.example", Uuid::new_v4().simple());
    let token = signup_and_login(
        app.clone(),
        &pool,
        &email,
        "correct-horse-battery-staple",
        "Me Endpoint Tenant",
        true,
    )
    .await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/me")
                .header("cookie", format!("rb_session={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK, "GET /v1/me must return 200");

    let raw = collect_body(resp.into_body()).await;
    let body: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(body["user"]["email"], email);
    assert_eq!(body["user"]["status"], "active");
    assert_eq!(body["user"]["email_verified"], true);
    assert!(body["user"]["created_at"].is_string());
    assert_eq!(body["current_tenant"]["name"], "Me Endpoint Tenant");
    assert_eq!(body["current_tenant"]["role"], "owner");
    assert!(body["current_tenant"]["slug"].is_string());
    let avail = body["available_tenants"].as_array().expect("array");
    assert_eq!(avail.len(), 1);
    assert_eq!(avail[0]["role"], "owner");
}

#[tokio::test]
async fn integration_me_extends_session_last_seen_at() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let app = build_public(state);
    let email = format!("integ-me-refresh-{}@test.example", Uuid::new_v4().simple());
    let token = signup_and_login(
        app.clone(),
        &pool,
        &email,
        "correct-horse-battery-staple",
        "Refresh Tenant",
        true,
    )
    .await;

    // Capture last_seen_at BEFORE the GET. Sleep briefly so the post-GET
    // refresh produces a strictly later timestamp.
    let token_hash = rb_auth::sha256_hex(&token);
    let before: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT last_seen_at FROM control.sessions WHERE token_hash = $1")
            .bind(&token_hash)
            .fetch_one(&pool)
            .await
            .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/me")
                .header("cookie", format!("rb_session={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Fire-and-forget refresh runs on a spawned task — wait for it to land.
    let mut after = before;
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        after =
            sqlx::query_scalar("SELECT last_seen_at FROM control.sessions WHERE token_hash = $1")
                .bind(&token_hash)
                .fetch_one(&pool)
                .await
                .unwrap();
        if after > before {
            break;
        }
    }
    assert!(
        after > before,
        "GET /v1/me must extend last_seen_at (before={before}, after={after})"
    );
}

#[tokio::test]
async fn integration_me_returns_session_expired_for_expired_cookie() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let app = build_public(state);
    let email = format!("integ-me-expired-{}@test.example", Uuid::new_v4().simple());
    let token = signup_and_login(
        app.clone(),
        &pool,
        &email,
        "correct-horse-battery-staple",
        "Expired Session Tenant",
        true,
    )
    .await;

    // Force the session to be expired.
    let token_hash = rb_auth::sha256_hex(&token);
    sqlx::query(
        "UPDATE control.sessions SET expires_at = now() - interval '1 hour' \
         WHERE token_hash = $1",
    )
    .bind(&token_hash)
    .execute(&pool)
    .await
    .expect("expiry patch must succeed");

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/me")
                .header("cookie", format!("rb_session={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let raw = collect_body(resp.into_body()).await;
    let body: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(body["error"], "session_expired");
}

#[tokio::test]
async fn integration_me_anonymous_returns_unauthorized() {
    let Some((state, _pool)) = real_db_state().await else {
        return;
    };
    let app = build_public(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/me")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let raw = collect_body(resp.into_body()).await;
    let body: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(body["error"], "unauthorized");
}

#[tokio::test]
async fn integration_me_with_unverified_email_returns_email_not_verified() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let app = build_public(state);
    let email = format!(
        "integ-me-unverified-{}@test.example",
        Uuid::new_v4().simple()
    );
    // verify_email=false: signup creates user with email_verified_at = NULL,
    // and login still succeeds (returning email_verification_required: true).
    let token = signup_and_login(
        app.clone(),
        &pool,
        &email,
        "correct-horse-battery-staple",
        "Unverified Tenant",
        false,
    )
    .await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/me")
                .header("cookie", format!("rb_session={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let raw = collect_body(resp.into_body()).await;
    let body: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(body["error"], "email_not_verified");
}
