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
        tempo_base_url: "http://localhost:3000".to_owned(),
        chat_panel_enabled: false,
        mcp_jwt_secret: Some("test-mcp-jwt-secret-for-unit-tests-only".to_owned()),
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
        internal_secret: "test-internal-secret".to_owned(),
        session_create_rate_limiter: Arc::new(SessionCreateRateLimiter::default()),
        tenant_session_count: Arc::new(TenantSessionCount::new()),
        mcp_jwt_secret: "test-mcp-jwt-secret".to_owned(),
        mcp_jwt_ttl_secs: 900,
        llm_api_key: String::new(),
        reranker: None,
        llm_tenant_tokens: std::sync::Arc::new(control_api::TenantLlmTokenCounter::new()),
    };
    Some((state, pool))
}

fn json_body(v: &serde_json::Value) -> Body {
    Body::from(serde_json::to_vec(v).expect("serialise JSON"))
}

/// Full logout flow: signup → SQL-verify email → login → logout → assert
/// 204 + cookie cleared + session row revoked + `auth_events` row written.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn integration_logout_full_flow() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let app = build_public(state);
    let email = format!("integ-logout-{}@test.example", Uuid::new_v4().simple());
    let password = "correct-horse-battery-staple";

    // 1. Signup
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
                    "tenant_name": "Integration Logout Tenant",
                })))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "signup must return 201");

    // 2. Verify email directly (noop transport discards the verification email)
    sqlx::query("UPDATE control.users SET email_verified_at = NOW() WHERE email = $1")
        .bind(&email)
        .execute(&pool)
        .await
        .expect("email verification patch must succeed");

    // 3. Login → capture session cookie
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
    let login_set_cookie = resp
        .headers()
        .get("set-cookie")
        .expect("login must set rb_session cookie")
        .to_str()
        .unwrap()
        .to_owned();
    let token = login_set_cookie
        .split(';')
        .next()
        .and_then(|kv| kv.trim().strip_prefix("rb_session="))
        .expect("rb_session token must parse out of Set-Cookie")
        .to_owned();

    // 4. Logout with that cookie
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/logout")
                .header("cookie", format!("rb_session={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "logout must return 204"
    );

    let clear = resp
        .headers()
        .get("set-cookie")
        .expect("logout must Set-Cookie to clear rb_session")
        .to_str()
        .unwrap();
    assert!(clear.starts_with("rb_session=;"), "Set-Cookie was: {clear}");
    assert!(clear.contains("Max-Age=0"), "Set-Cookie was: {clear}");
    assert!(clear.contains("Secure"), "Set-Cookie was: {clear}");

    // 5. Session row revoked
    let token_hash = rb_auth::sha256_hex(&token);
    let revoked: bool = sqlx::query_scalar(
        "SELECT revoked_at IS NOT NULL FROM control.sessions WHERE token_hash = $1",
    )
    .bind(&token_hash)
    .fetch_one(&pool)
    .await
    .expect("session row must exist");
    assert!(revoked, "session must be revoked after logout");

    // 6. Logout audit event written exactly once for this user
    let logout_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::BIGINT FROM control.auth_events e \
         JOIN control.users u ON u.id = e.user_id \
         WHERE u.email = $1 AND e.event = 'logout'",
    )
    .bind(&email)
    .fetch_one(&pool)
    .await
    .expect("auth_events count must succeed");
    assert_eq!(
        logout_count, 1,
        "exactly one logout auth_events row expected"
    );

    // 7. Re-logout with the same (now invalid) cookie → 401, no duplicate event.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/logout")
                .header("cookie", format!("rb_session={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "second logout must reject the now-revoked session",
    );

    let logout_count_after: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::BIGINT FROM control.auth_events e \
         JOIN control.users u ON u.id = e.user_id \
         WHERE u.email = $1 AND e.event = 'logout'",
    )
    .bind(&email)
    .fetch_one(&pool)
    .await
    .expect("auth_events count must succeed");
    assert_eq!(
        logout_count_after, 1,
        "logout audit must remain idempotent after replay",
    );
}
