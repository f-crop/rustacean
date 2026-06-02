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

use control_api::{AppState, Config, build_public};
use rb_sse::{EventBus, SseConfig};

fn test_state() -> AppState {
    let config = Config::for_test();
    let pool = PgPoolOptions::new()
        .connect_lazy(&config.database_url)
        .expect("connect_lazy must succeed");
    let smtp = rb_email::SmtpConfig {
        host: String::new(),
        port: 587,
        username: String::new(),
        password: String::new(),
        from_address: "test@example.com".to_owned(),
    };
    let email_sender = from_transport("noop", &smtp).expect("noop transport must succeed");
    let hasher = PasswordHasher::from_config(64, 1, 1).expect("hasher must build");
    AppState {
        pool,
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
        session_create_rate_limiter: Arc::new(control_api::SessionCreateRateLimiter::new(10, 60)),
        tenant_session_count: Arc::new(control_api::TenantSessionCount::new()),
        mcp_jwt_secret: "test-mcp-jwt-secret".to_owned(),
        mcp_jwt_ttl_secs: 900,
    }
}

fn app() -> axum::Router {
    build_public(test_state())
}

async fn collect_body(body: Body) -> Vec<u8> {
    body.collect()
        .await
        .expect("collect body")
        .to_bytes()
        .to_vec()
}

#[tokio::test]
async fn health_returns_200() {
    let response = app()
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn ready_returns_200() {
    let response = app()
        .oneshot(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn openapi_json_content_type_is_application_json() {
    let response = app()
        .oneshot(
            Request::builder()
                .uri("/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .expect("content-type header must be present")
        .to_str()
        .unwrap();
    assert!(
        content_type.contains("application/json"),
        "expected application/json, got {content_type}"
    );
}

#[tokio::test]
async fn openapi_json_body_is_valid_openapi() {
    let response = app()
        .oneshot(
            Request::builder()
                .uri("/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let raw = collect_body(response.into_body()).await;
    let spec: serde_json::Value = serde_json::from_slice(&raw).expect("body must be valid JSON");

    let version = spec["openapi"]
        .as_str()
        .expect("'openapi' field must be a string");
    assert!(
        version.starts_with("3."),
        "expected OpenAPI 3.x, got {version}"
    );
    assert!(
        spec["info"].is_object(),
        "'info' must be present and an object"
    );
    assert!(
        spec["paths"].is_object(),
        "'paths' must be present and an object"
    );
}

#[tokio::test]
async fn openapi_json_includes_signup_path() {
    let response = app()
        .oneshot(
            Request::builder()
                .uri("/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let raw = collect_body(response.into_body()).await;
    let spec: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert!(
        spec["paths"]["/v1/auth/signup"].is_object(),
        "signup path must be present in OpenAPI spec"
    );
}

#[tokio::test]
async fn openapi_json_includes_verify_email_path() {
    let response = app()
        .oneshot(
            Request::builder()
                .uri("/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let raw = collect_body(response.into_body()).await;
    let spec: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert!(
        spec["paths"]["/v1/auth/verify-email"].is_object(),
        "verify-email path must be present in OpenAPI spec"
    );
}

#[tokio::test]
async fn verify_email_without_body_returns_400() {
    let response = app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/verify-email")
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Axum returns 400 when the JSON body is absent/empty (cannot parse EOF as JSON).
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn verify_email_missing_token_field_returns_422() {
    let response = app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/verify-email")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn unknown_route_returns_404() {
    let response = app()
        .oneshot(
            Request::builder()
                .uri("/does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn openapi_json_includes_logout_path() {
    let response = app()
        .oneshot(
            Request::builder()
                .uri("/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let raw = collect_body(response.into_body()).await;
    let spec: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert!(
        spec["paths"]["/v1/auth/logout"]["post"].is_object(),
        "logout POST must be present in OpenAPI spec",
    );
}

#[tokio::test]
async fn logout_without_session_cookie_returns_401() {
    // No `Cookie` header → AuthContext::Anonymous resolves without touching the
    // database, so this exercises the unauthorized branch end-to-end without a
    // running Postgres.
    let response = app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/logout")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logout_rejects_get_method() {
    let response = app()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/auth/logout")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}

// ---------------------------------------------------------------------------
// Real-DB integration tests — skipped when RB_DATABASE_URL is not set
// ---------------------------------------------------------------------------

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
        session_create_rate_limiter: Arc::new(control_api::SessionCreateRateLimiter::new(10, 60)),
        tenant_session_count: Arc::new(control_api::TenantSessionCount::new()),
        mcp_jwt_secret: "test-mcp-jwt-secret".to_owned(),
        mcp_jwt_ttl_secs: 900,
    };
    Some((state, pool))
}

fn json_body(v: &serde_json::Value) -> Body {
    Body::from(serde_json::to_vec(v).expect("serialise JSON"))
}

/// signup → SQL-verify email → login → assert Secure cookie present.
#[tokio::test]
async fn integration_login_full_flow() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let app = build_public(state);
    let email = format!("integ-login-{}@test.example", Uuid::new_v4().simple());
    let password = "correct-horse-battery-staple";

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
                    "tenant_name": "Integration Login Tenant",
                })))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "signup must return 201");

    // noop transport discards the email — patch verified_at directly
    sqlx::query("UPDATE control.users SET email_verified_at = NOW() WHERE email = $1")
        .bind(&email)
        .execute(&pool)
        .await
        .expect("email verification patch must succeed");

    let resp = app
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
    assert_eq!(resp.status(), StatusCode::OK, "login must return 200");

    let cookie = resp
        .headers()
        .get("set-cookie")
        .expect("Set-Cookie header must be present")
        .to_str()
        .unwrap();
    assert!(
        cookie.contains("rb_session="),
        "cookie must contain rb_session token"
    );
    assert!(
        cookie.contains("Secure"),
        "cookie must carry the Secure flag"
    );
    assert!(cookie.contains("HttpOnly"), "cookie must carry HttpOnly");

    let raw = collect_body(resp.into_body()).await;
    let body: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert!(body["user_id"].is_string(), "user_id must be present");
    assert!(body["tenant_id"].is_string(), "tenant_id must be present");
    assert_eq!(
        body["email_verification_required"], false,
        "email_verification_required must be false after verification"
    );
}

/// Rate-limit path: 5 failed attempts → 6th is blocked with 429.
#[tokio::test]
async fn integration_login_rate_limit() {
    let Some((state, _pool)) = real_db_state().await else {
        return;
    };
    let app = build_public(state);
    let email = format!("integ-ratelimit-{}@test.example", Uuid::new_v4().simple());
    let password = "correct-horse-battery-staple";

    // Signup so the email resolves in the DB (rate limiter fires on bad password,
    // not on "user not found" — both paths record a failure, but this makes the
    // argon2 verify path exercise the full record_attempt logic).
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
                    "tenant_name": "Rate Limit Test Tenant",
                })))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // 5 wrong-password attempts — each must return 401
    for i in 0..5 {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/auth/login")
                    .header("content-type", "application/json")
                    .body(json_body(&serde_json::json!({
                        "email": email,
                        "password": "wrong-password-xyz",
                    })))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "attempt {i} must be 401"
        );
    }

    // 6th attempt — must be rate-limited (429) regardless of credentials
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/login")
                .header("content-type", "application/json")
                .body(json_body(&serde_json::json!({
                    "email": email,
                    "password": "wrong-password-xyz",
                })))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "6th attempt must be rate-limited"
    );
}

/// Signup response must set `rb_session` cookie with Secure + `HttpOnly` flags.
#[tokio::test]
async fn integration_signup_cookie_has_secure_flag() {
    let Some((state, _pool)) = real_db_state().await else {
        return;
    };
    let app = build_public(state);
    let email = format!(
        "integ-signup-secure-{}@test.example",
        Uuid::new_v4().simple()
    );
    let password = "correct-horse-battery-staple";

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/signup")
                .header("content-type", "application/json")
                .body(json_body(&serde_json::json!({
                    "email": email,
                    "password": password,
                    "tenant_name": "Secure Cookie Test Tenant",
                })))
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
