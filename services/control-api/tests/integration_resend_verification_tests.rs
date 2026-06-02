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
        llm_api_key: String::new(),
    };
    Some((state, pool))
}

fn json_body(v: &serde_json::Value) -> Body {
    Body::from(serde_json::to_vec(v).expect("serialise JSON"))
}

/// `POST /v1/auth/resend-verification` returns 204 for an unverified user.
/// A new token row is written and the old unused token is expired.
#[tokio::test]
async fn integration_resend_verification_unverified_user() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let app = build_public(state);
    let email = format!("integ-resend-{}@test.example", Uuid::new_v4().simple());
    let password = "correct-horse-battery-staple";

    // 1. Signup (noop transport — auto-verified in noop mode, but we'll manually
    //    un-verify to test the resend path).
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
                    "tenant_name": "Resend Test Tenant",
                })))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "signup must return 201");

    // 2. Force the user to unverified state to exercise the resend path.
    sqlx::query("UPDATE control.users SET email_verified_at = NULL WHERE email = $1")
        .bind(&email)
        .execute(&pool)
        .await
        .expect("unverify patch must succeed");

    // 3. Count existing unused verify tokens before resend.
    let tokens_before: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::BIGINT \
         FROM control.email_tokens et \
         JOIN control.users u ON u.id = et.user_id \
         WHERE u.email = $1 AND et.kind = 'verify' AND et.used_at IS NULL AND et.expires_at > now()",
    )
    .bind(&email)
    .fetch_one(&pool)
    .await
    .expect("token count must succeed");

    // 4. Call resend-verification.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/resend-verification")
                .header("content-type", "application/json")
                .body(json_body(&serde_json::json!({ "email": email })))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "resend must return 204"
    );

    // 5. Old tokens expired, exactly one new token active.
    let tokens_after: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::BIGINT \
         FROM control.email_tokens et \
         JOIN control.users u ON u.id = et.user_id \
         WHERE u.email = $1 AND et.kind = 'verify' AND et.used_at IS NULL AND et.expires_at > now()",
    )
    .bind(&email)
    .fetch_one(&pool)
    .await
    .expect("token count must succeed");
    assert_eq!(
        tokens_after, 1,
        "exactly one active verify token expected after resend"
    );

    // Old tokens from before the resend must all be expired now.
    if tokens_before > 0 {
        let still_valid: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::BIGINT \
             FROM control.email_tokens et \
             JOIN control.users u ON u.id = et.user_id \
             WHERE u.email = $1 AND et.kind = 'verify' AND et.used_at IS NULL \
               AND et.expires_at > now() \
             ORDER BY et.created_at ASC \
             LIMIT $2",
        )
        .bind(&email)
        .bind(tokens_before)
        .fetch_one(&pool)
        .await
        .expect("stale token check must succeed");
        // The one valid token is the freshly issued one, not the old ones.
        assert_eq!(still_valid, 1);
    }

    // 6. verification_resent auth event was written.
    let event_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::BIGINT FROM control.auth_events e \
         JOIN control.users u ON u.id = e.user_id \
         WHERE u.email = $1 AND e.event = 'verification_resent'",
    )
    .bind(&email)
    .fetch_one(&pool)
    .await
    .expect("auth_events count must succeed");
    assert_eq!(
        event_count, 1,
        "exactly one verification_resent event expected"
    );
}

/// Resend for an unknown email returns 204 (no email enumeration).
#[tokio::test]
async fn integration_resend_verification_unknown_email_returns_204() {
    let Some((state, _pool)) = real_db_state().await else {
        return;
    };
    let app = build_public(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/resend-verification")
                .header("content-type", "application/json")
                .body(json_body(
                    &serde_json::json!({ "email": "nobody@does-not-exist.example" }),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "unknown email must still return 204"
    );
}

/// Resend for an already-verified user returns 204 (no enumeration).
#[tokio::test]
async fn integration_resend_verification_already_verified_returns_204() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let app = build_public(state);
    let email = format!(
        "integ-resend-verified-{}@test.example",
        Uuid::new_v4().simple()
    );
    let password = "correct-horse-battery-staple";

    // Signup (noop transport auto-verifies).
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
                    "tenant_name": "Already Verified Tenant",
                })))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Ensure verified.
    sqlx::query("UPDATE control.users SET email_verified_at = now() WHERE email = $1")
        .bind(&email)
        .execute(&pool)
        .await
        .expect("verify patch must succeed");

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/resend-verification")
                .header("content-type", "application/json")
                .body(json_body(&serde_json::json!({ "email": email })))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "already-verified email must return 204 silently"
    );
}

/// Dev-mode auto-verify: signup with noop transport returns `email_verification_required: false`.
#[tokio::test]
async fn integration_signup_noop_transport_auto_verifies() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let app = build_public(state);
    let email = format!("integ-autoverify-{}@test.example", Uuid::new_v4().simple());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/signup")
                .header("content-type", "application/json")
                .body(json_body(&serde_json::json!({
                    "email": email,
                    "password": "correct-horse-battery-staple",
                    "tenant_name": "Auto Verify Tenant",
                })))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body must read");
    let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(
        body["email_verification_required"], false,
        "noop transport must auto-verify: got {body:?}"
    );

    // DB row must have email_verified_at set.
    let verified: bool = sqlx::query_scalar(
        "SELECT (email_verified_at IS NOT NULL) FROM control.users WHERE email = $1",
    )
    .bind(&email)
    .fetch_one(&pool)
    .await
    .expect("user row must exist");
    assert!(
        verified,
        "email_verified_at must be set for noop transport signup"
    );

    // Login immediately after signup must not require verification.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/login")
                .header("content-type", "application/json")
                .body(json_body(&serde_json::json!({
                    "email": email,
                    "password": "correct-horse-battery-staple",
                })))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body must read");
    let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(
        body["email_verification_required"], false,
        "login after noop signup must not require verification: got {body:?}"
    );
}
