//! DB-backed integration tests for admin v1 audit-log invariants (ADR-012 §S1.6).
//!
//!   §S1.6.1 — Every endpoint writes exactly one audit row on every code path.
//!   §S1.6.2 — Missing `X-Admin-Actor` → 400 + denied/missing_actor audit row.
//!   §S1.6.3 — `payload_summary` never contains raw token material.
//!   §S1.6.4 — Impersonation JWT `exp` ≤ `now + 900 s` (server-enforced ceiling).
//!   §S1.6.5 — force-delete is two-phase: phase-1 returns confirm_token; phase-2
//!              executes only when confirm_token is present and valid.
//!
//! Fast middleware tests (no DB) live in `integration_admin_v1_tests.rs`.
//! All tests here skip when `RB_DATABASE_URL` is unset.

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
    }
}

/// Returns `None` when `RB_DATABASE_URL` is unset, silently skipping DB tests.
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
// §S1.6 audit invariant tests
// ---------------------------------------------------------------------------

/// §S1.6.2 — Missing `X-Admin-Actor` must write exactly one audit row with
/// `outcome='denied'` and `error_class='missing_actor'`.
#[tokio::test]
async fn inv2_missing_actor_writes_denied_audit_row() {
    let Some(pool) = real_db_pool().await else {
        return;
    };

    let config = lazy_config_with_token(&std::env::var("RB_DATABASE_URL").unwrap());
    let app = build_public(build_state_from_pool(pool.clone(), config));

    let request_id = Uuid::new_v4();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/v1/bootstrap/admin")
                .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
                .header("x-request-id", request_id.to_string())
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"email":"inv2@test.com","password":"supersecret12"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // Must be 400 — missing actor
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "missing actor must return 400"
    );

    // §S1.6.2: exactly one audit row with denied/missing_actor
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT outcome, error_class \
         FROM auth.admin_audit_log \
         WHERE request_id = $1",
    )
    .bind(request_id)
    .fetch_optional(&pool)
    .await
    .expect("query audit row");

    let (outcome, error_class) = row.expect("audit row must exist for missing actor");
    assert_eq!(outcome, "denied", "outcome must be 'denied'");
    assert_eq!(
        error_class, "missing_actor",
        "error_class must be 'missing_actor'"
    );
}

/// §S1.6.1 — Bootstrap endpoint writes exactly one audit row per call,
/// even on the 409 conflict path.
#[tokio::test]
async fn inv1_bootstrap_writes_exactly_one_audit_row_on_any_path() {
    let Some(pool) = real_db_pool().await else {
        return;
    };

    let config = lazy_config_with_token(&std::env::var("RB_DATABASE_URL").unwrap());
    let app = build_public(build_state_from_pool(pool.clone(), config));

    let request_id = Uuid::new_v4();

    // POST bootstrap — if users exist it returns 409 (conflict), still writes row.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/v1/bootstrap/admin")
                .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
                .header("x-admin-actor", "inv1-test-actor")
                .header("x-request-id", request_id.to_string())
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"email":"inv1@test.com","password":"supersecret12"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // Either 201 (first bootstrap) or 409 (already bootstrapped).
    assert!(
        resp.status() == StatusCode::CREATED || resp.status() == StatusCode::CONFLICT,
        "bootstrap must return 201 or 409, got {}",
        resp.status()
    );

    // §S1.6.1: exactly ONE audit row for this request_id
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM auth.admin_audit_log WHERE request_id = $1")
            .bind(request_id)
            .fetch_one(&pool)
            .await
            .expect("count audit rows");

    assert_eq!(
        count, 1,
        "bootstrap must write exactly one audit row, got {count}"
    );
}

/// §S1.6.3 — `payload_summary` in audit rows must never contain the raw admin token.
#[tokio::test]
async fn inv3_audit_payload_never_contains_raw_token() {
    let Some(pool) = real_db_pool().await else {
        return;
    };

    let config = lazy_config_with_token(&std::env::var("RB_DATABASE_URL").unwrap());
    let app = build_public(build_state_from_pool(pool.clone(), config));

    let request_id = Uuid::new_v4();

    // Hit bootstrap (409 expected on a non-empty DB, but either path writes row)
    let _resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/v1/bootstrap/admin")
                .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
                .header("x-admin-actor", "inv3-actor")
                .header("x-request-id", request_id.to_string())
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"email":"inv3@test.com","password":"supersecret12"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let payload: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT payload_summary FROM auth.admin_audit_log WHERE request_id = $1",
    )
    .bind(request_id)
    .fetch_optional(&pool)
    .await
    .expect("query payload_summary");

    let payload = payload.expect("audit row must exist");
    let payload_str = payload.to_string();

    // §S1.6.3: raw token must not appear in payload_summary
    assert!(
        !payload_str.contains(ADMIN_TOKEN),
        "payload_summary must not contain raw admin token, got: {payload_str}"
    );
}

/// §S1.6.4 — Impersonation JWT `exp` must be ≤ `now + 900 s` even when the
/// caller requests a longer duration (e.g. 9999 seconds).
#[tokio::test]
async fn inv4_impersonation_jwt_exp_ceiling_is_900s() {
    let Some(pool) = real_db_pool().await else {
        return;
    };

    // Seed a tenant + user membership for the impersonation test.
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let slug = format!("adminv1-imp-{}", tenant_id.simple());

    sqlx::query("INSERT INTO control.tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind("admin-v1 impersonate test")
        .bind(&slug)
        .execute(&pool)
        .await
        .expect("insert tenant");

    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, status) \
         VALUES ($1, $2, 'x', 'active')",
    )
    .bind(user_id)
    .bind(format!("inv4-{}@test.com", Uuid::new_v4()))
    .execute(&pool)
    .await
    .expect("insert user");

    sqlx::query(
        "INSERT INTO control.tenant_members (tenant_id, user_id, role) \
         VALUES ($1, $2, 'member')",
    )
    .bind(tenant_id)
    .bind(user_id)
    .execute(&pool)
    .await
    .expect("insert member");

    let config = lazy_config_with_token(&std::env::var("RB_DATABASE_URL").unwrap());
    let app = build_public(build_state_from_pool(pool.clone(), config));

    let before_secs = chrono::Utc::now().timestamp();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/admin/v1/tenants/{tenant_id}/impersonate"))
                .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
                .header("x-admin-actor", "inv4-actor")
                .header("content-type", "application/json")
                // Request 9999 seconds — server must clamp to 900.
                .body(Body::from(format!(
                    r#"{{"user_id":"{}","duration_secs":9999}}"#,
                    user_id
                )))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK, "impersonate must succeed");

    let body = collect_body(resp.into_body()).await;
    let token = body["token"]
        .as_str()
        .expect("response must have token field");

    // Decode without verification to read claims (we don't have the derived key).
    let header = jsonwebtoken::decode_header(token).expect("decode header");
    assert_eq!(header.alg, jsonwebtoken::Algorithm::HS256);

    // Extract exp from the JWT payload without verifying the signature.
    let parts: Vec<&str> = token.split('.').collect();
    assert_eq!(parts.len(), 3, "JWT must have 3 parts");
    use base64::Engine as _;
    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .expect("decode payload base64");
    let claims: serde_json::Value =
        serde_json::from_slice(&payload_bytes).expect("decode claims JSON");

    let exp = claims["exp"].as_i64().expect("exp must be integer");
    let typ = claims["typ"].as_str().expect("typ must be string");

    assert_eq!(typ, "imp", "token type must be 'imp'");

    // §S1.6.4: exp ≤ now + 900 s
    let ceiling = before_secs + 900 + 2; // +2s for clock jitter
    assert!(
        exp <= ceiling,
        "§S1.6.4: JWT exp {exp} must be ≤ now+900s ({ceiling})"
    );

    // Cleanup
    sqlx::query("DELETE FROM control.tenant_members WHERE tenant_id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.users WHERE id = $1")
        .bind(user_id)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}

/// §S1.6.5 — force-delete is two-phase: phase-1 (no confirm_token) returns a
/// `confirm_token` and snapshot; phase-2 requires the confirm_token.
/// This test verifies that the phase-1 endpoint returns the expected shape
/// and that a phase-2 call without a valid confirm_token is rejected.
#[tokio::test]
async fn inv5_force_delete_is_two_phase() {
    let Some(pool) = real_db_pool().await else {
        return;
    };

    // Seed a minimal tenant.
    let tenant_id = Uuid::new_v4();
    let slug = format!("adminv1-fd-{}", tenant_id.simple());

    sqlx::query("INSERT INTO control.tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind("admin-v1 force-delete test")
        .bind(&slug)
        .execute(&pool)
        .await
        .expect("insert tenant for force-delete test");

    let db_url = std::env::var("RB_DATABASE_URL").unwrap();
    let config = lazy_config_with_token(&db_url);

    // Phase 1: no confirm_token — must return 200 with confirm_token + snapshot.
    let phase1_resp = build_public(build_state_from_pool(pool.clone(), config.clone()))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/admin/v1/tenants/{tenant_id}/force-delete"))
                .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
                .header("x-admin-actor", "inv5-actor")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        phase1_resp.status(),
        StatusCode::OK,
        "phase-1 force-delete must return 200"
    );

    let phase1_body = collect_body(phase1_resp.into_body()).await;
    assert!(
        phase1_body["confirm_token"].is_string(),
        "§S1.6.5: phase-1 must return confirm_token, got: {phase1_body}"
    );
    assert!(
        phase1_body["snapshot"].is_object(),
        "§S1.6.5: phase-1 must return snapshot, got: {phase1_body}"
    );

    // Phase 2 with an invalid confirm_token must be rejected.
    let phase2_bad_resp = build_public(build_state_from_pool(pool.clone(), config))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/admin/v1/tenants/{tenant_id}/force-delete"))
                .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
                .header("x-admin-actor", "inv5-actor")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"confirm_token":"invalid.token.here"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        phase2_bad_resp.status() == StatusCode::BAD_REQUEST
            || phase2_bad_resp.status() == StatusCode::UNAUTHORIZED,
        "§S1.6.5: phase-2 with invalid confirm_token must be rejected, got {}",
        phase2_bad_resp.status()
    );

    // Cleanup
    sqlx::query("DELETE FROM control.tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}

/// Handler-level audit rows must carry the client IP and user-agent that the
/// middleware extracted and injected as extensions.
///
/// Hits `POST /api/admin/v1/bootstrap/admin` (either 201 or 409) and verifies
/// that the resulting audit row has non-NULL `ip` and `user_agent` columns.
#[tokio::test]
async fn handler_audit_row_carries_ip_and_user_agent() {
    let Some(pool) = real_db_pool().await else {
        return;
    };

    let config = lazy_config_with_token(&std::env::var("RB_DATABASE_URL").unwrap());
    let app = build_public(build_state_from_pool(pool.clone(), config));

    let request_id = Uuid::new_v4();

    let _resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/v1/bootstrap/admin")
                .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
                .header("x-admin-actor", "ip-ua-test-actor")
                .header("x-request-id", request_id.to_string())
                .header("x-forwarded-for", "198.51.100.42")
                .header("user-agent", "test-harness/1.0")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"email":"ipua@test.com","password":"supersecret12"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT ip::text, user_agent \
         FROM auth.admin_audit_log \
         WHERE request_id = $1",
    )
    .bind(request_id)
    .fetch_optional(&pool)
    .await
    .expect("query audit row");

    let (ip, ua) = row.expect("audit row must exist");
    assert!(
        ip.is_some(),
        "handler audit row must have non-NULL ip, got None"
    );
    assert!(
        ua.is_some(),
        "handler audit row must have non-NULL user_agent, got None"
    );
    assert!(
        ip.as_deref().unwrap_or("").contains("198.51.100.42"),
        "ip must contain the forwarded address, got: {ip:?}"
    );
    assert_eq!(
        ua.as_deref(),
        Some("test-harness/1.0"),
        "user_agent must match the sent header"
    );
}
