//! RUSAA-1801 — integration test: GET /api/admin/v1/audit-log surfaces ip + `user_agent`.
//!
//! Verifies that the two fields added to `AuditLogRow` round-trip from the DB
//! through the GET response DTO.
//!
//! Skips gracefully when `RB_DATABASE_URL` is unset.

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
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Shared helpers (intentionally local — test files are independent binaries)
// ---------------------------------------------------------------------------

const ADMIN_TOKEN: &str = "admin-integration-test-token-secure-32x";

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
    }
}

async fn real_db_pool() -> Option<PgPool> {
    let db_url = std::env::var("RB_DATABASE_URL").ok()?;
    PgPoolOptions::new()
        .max_connections(3)
        .connect(&db_url)
        .await
        .ok()
}

async fn collect_body(body: Body) -> serde_json::Value {
    let bytes = body.collect().await.expect("collect body").to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

// ---------------------------------------------------------------------------
// RUSAA-1801: GET /api/admin/v1/audit-log must surface ip + user_agent
// ---------------------------------------------------------------------------

/// Seeds a known audit row by hitting the bootstrap endpoint, then queries
/// `GET /api/admin/v1/audit-log` and asserts the two fields round-trip
/// correctly in the JSON response.
#[tokio::test]
async fn audit_log_get_surfaces_ip_and_user_agent() {
    let Some(pool) = real_db_pool().await else {
        return;
    };

    let config = lazy_config_with_token(&std::env::var("RB_DATABASE_URL").unwrap());
    let app = build_public(build_state_from_pool(pool.clone(), config));

    let request_id = Uuid::new_v4();
    let test_ip = "203.0.113.7";
    let test_ua = "RUSAA-1801-test/1.0";

    // Seed a row via bootstrap (either 201 or 409 — both write an audit row).
    let _ = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/v1/bootstrap/admin")
                .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
                .header("x-admin-actor", "rusaa1801-actor")
                .header("x-request-id", request_id.to_string())
                .header("x-forwarded-for", test_ip)
                .header("user-agent", test_ua)
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"email":"rusaa1801@test.com","password":"supersecret12"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // Confirm the row landed in the DB.
    let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT ip::text, user_agent FROM auth.admin_audit_log WHERE request_id = $1",
    )
    .bind(request_id)
    .fetch_optional(&pool)
    .await
    .expect("query seeded audit row");

    let (db_ip, db_ua) = row.expect("seeded audit row must exist");
    assert!(
        db_ip.as_deref().unwrap_or("").contains(test_ip),
        "DB ip must contain seeded IP, got: {db_ip:?}"
    );
    assert_eq!(
        db_ua.as_deref(),
        Some(test_ua),
        "DB user_agent must match seeded UA"
    );

    // Query the GET endpoint and verify `ip` + `user_agent` appear in the JSON.
    let config2 = lazy_config_with_token(&std::env::var("RB_DATABASE_URL").unwrap());
    let app2 = build_public(build_state_from_pool(pool.clone(), config2));

    let resp = app2
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/v1/audit-log?limit=500")
                .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
                .header("x-admin-actor", "rusaa1801-query-actor")
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "audit-log GET must return 200"
    );

    let body = collect_body(resp.into_body()).await;
    let rows = body["rows"]
        .as_array()
        .expect("response must have rows array");

    // Find the seeded row by request_id.
    let rid_str = request_id.to_string();
    let seeded = rows
        .iter()
        .find(|r| r["request_id"].as_str().is_some_and(|s| s == rid_str));
    let seeded = seeded.expect("seeded row must appear in GET /api/admin/v1/audit-log response");

    assert!(
        seeded["ip"].is_string(),
        "ip field must be a string in response row, got: {:?}",
        seeded["ip"]
    );
    assert!(
        seeded["user_agent"].is_string(),
        "user_agent field must be a string in response row, got: {:?}",
        seeded["user_agent"]
    );
    assert!(
        seeded["ip"].as_str().unwrap_or("").contains(test_ip),
        "response ip must contain the seeded IP, got: {:?}",
        seeded["ip"]
    );
    assert_eq!(
        seeded["user_agent"].as_str(),
        Some(test_ua),
        "response user_agent must match the seeded UA"
    );
}
