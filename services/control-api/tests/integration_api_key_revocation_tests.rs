//! Integration tests — API key revocation on session termination (RUSAA-1189).
//!
//! Acceptance criteria:
//!   AC1: `PATCH /internal/agent/sessions/{id}/status` with a terminal status
//!        sets `control.api_keys.revoked_at` within the same request.
//!   AC2: `DELETE /v1/agents/sessions/{id}` for a pending session (sync cancel)
//!        also revokes the API key.
//!   AC3: Revocation is idempotent — a second terminal status patch does not
//!        error and does not clear `revoked_at`.
//!
//! Tests require a running Postgres instance at `RB_DATABASE_URL`; they skip
//! gracefully when that variable is absent.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use rb_auth::{LoginRateLimiter, PasswordHasher, sha256_hex};
use rb_email::from_transport;
use rb_sse::{EventBus, SseConfig};
use serde_json::json;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt as _;
use uuid::Uuid;

use control_api::{
    AppState, Config, SessionCreateRateLimiter, TenantSessionCount, build_internal, build_public,
};

const INTERNAL_SECRET: &str = "test-revoke-internal-secret";

// ---------------------------------------------------------------------------
// State builder
// ---------------------------------------------------------------------------

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
        service_name: "control-api-revoke-test".to_owned(),
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
        internal_secret: Some(INTERNAL_SECRET.to_owned()),
        internal_listen_addr: "127.0.0.1:0".to_owned(),
        session_create_rate_limit: 10,
        session_create_window_secs: 60,
        tenant_session_cap: 100,
        admin_token: None,
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
        internal_secret: INTERNAL_SECRET.to_owned(),
        session_create_rate_limiter: Arc::new(SessionCreateRateLimiter::default()),
        tenant_session_count: Arc::new(TenantSessionCount::new()),
    };
    Some((state, pool))
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

struct Fixtures {
    agent_session_id: Uuid,
    api_key_id: Uuid,
    tenant_id: Uuid,
    /// A valid control-plane session token for the fixture user (for DELETE tests).
    session_token: String,
}

/// Insert the minimum rows needed to test API-key revocation:
///   tenant → user → `control.sessions` → `control.api_keys` → `agents.agent_sessions`
async fn insert_fixtures(pool: &PgPool) -> Fixtures {
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let ctrl_session_id = Uuid::new_v4();
    let agent_session_id = Uuid::new_v4();
    let api_key_id = Uuid::new_v4();

    let slug = format!("revoke-test-{}", tenant_id.simple());
    let schema_name = format!("revoke_{}", tenant_id.simple());

    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(&slug)
    .bind("Revoke Test Tenant")
    .bind(&schema_name)
    .execute(pool)
    .await
    .expect("insert tenant");

    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, email_verified_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(user_id)
    .bind(format!("revoke-{}@test.example", user_id.simple()))
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

    let session_token = format!("revoke-test-token-{}", Uuid::new_v4().simple());
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

    // Session-scoped API key (simulating what create_session inserts).
    let key_hash = sha256_hex(&format!("rbk_{}", api_key_id.simple()));
    let scopes = json!(["agent"]);
    sqlx::query(
        "INSERT INTO control.api_keys \
         (id, tenant_id, key_hash, name, scopes, created_by_user_id) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(api_key_id)
    .bind(tenant_id)
    .bind(&key_hash)
    .bind(format!("agent-session-{agent_session_id}"))
    .bind(&scopes)
    .bind(user_id)
    .execute(pool)
    .await
    .expect("insert api_key");

    // Agent session row in 'pending' status with api_key_id wired.
    sqlx::query(
        r"INSERT INTO agents.agent_sessions
          (id, tenant_id, user_id, runtime_kind, model, system_prompt,
           status, token_budget, tokens_used, input_prompt_preview,
           workspace_path, api_key_id, created_at)
          VALUES ($1, $2, $3, 'claude_code', 'n/a', '',
                  'pending', 100000, 0, 'test prompt',
                  $4, $5, now())",
    )
    .bind(agent_session_id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(format!("{tenant_id}/{agent_session_id}"))
    .bind(api_key_id)
    .execute(pool)
    .await
    .expect("insert agent_session");

    Fixtures {
        agent_session_id,
        api_key_id,
        tenant_id,
        session_token,
    }
}

async fn api_key_revoked_at(
    pool: &PgPool,
    api_key_id: Uuid,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let row: Option<(Option<chrono::DateTime<chrono::Utc>>,)> =
        sqlx::query_as("SELECT revoked_at FROM control.api_keys WHERE id = $1")
            .bind(api_key_id)
            .fetch_optional(pool)
            .await
            .expect("query revoked_at");
    row.and_then(|(ts,)| ts)
}

// ---------------------------------------------------------------------------
// AC1 — PATCH terminal status revokes the API key
// ---------------------------------------------------------------------------

/// `PATCH /internal/.../status` with `failed` sets `revoked_at` on the
/// session-scoped API key within the same request.
#[tokio::test]
async fn ac1a_patch_failed_status_revokes_api_key() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fix = insert_fixtures(&pool).await;

    let body = json!({
        "status": "failed",
        "tenant_id": fix.tenant_id,
        "error": "spawn failed"
    });

    let resp = build_internal(state)
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!(
                    "/internal/agent/sessions/{}/status",
                    fix.agent_session_id
                ))
                .header("content-type", "application/json")
                .header("x-internal-secret", INTERNAL_SECRET)
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "PATCH should return 204"
    );

    let revoked_at = api_key_revoked_at(&pool, fix.api_key_id).await;
    assert!(
        revoked_at.is_some(),
        "AC1a: api_key.revoked_at must be set after 'failed' status patch"
    );
}

/// `PATCH /internal/.../status` with `terminated` revokes the API key.
#[tokio::test]
async fn ac1b_patch_terminated_status_revokes_api_key() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fix = insert_fixtures(&pool).await;

    // Move to 'running' first so 'terminated' is a valid transition.
    sqlx::query("UPDATE agents.agent_sessions SET status = 'running' WHERE id = $1")
        .bind(fix.agent_session_id)
        .execute(&pool)
        .await
        .unwrap();

    let body = json!({
        "status": "terminated",
        "tenant_id": fix.tenant_id,
    });

    let resp = build_internal(state)
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!(
                    "/internal/agent/sessions/{}/status",
                    fix.agent_session_id
                ))
                .header("content-type", "application/json")
                .header("x-internal-secret", INTERNAL_SECRET)
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "PATCH should return 204"
    );

    let revoked_at = api_key_revoked_at(&pool, fix.api_key_id).await;
    assert!(
        revoked_at.is_some(),
        "AC1b: api_key.revoked_at must be set after 'terminated' status patch"
    );
}

/// `PATCH /internal/.../status` with `cancelled` revokes the API key.
#[tokio::test]
async fn ac1c_patch_cancelled_status_revokes_api_key() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fix = insert_fixtures(&pool).await;

    let body = json!({
        "status": "cancelled",
        "tenant_id": fix.tenant_id,
    });

    let resp = build_internal(state)
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!(
                    "/internal/agent/sessions/{}/status",
                    fix.agent_session_id
                ))
                .header("content-type", "application/json")
                .header("x-internal-secret", INTERNAL_SECRET)
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "PATCH should return 204"
    );

    let revoked_at = api_key_revoked_at(&pool, fix.api_key_id).await;
    assert!(
        revoked_at.is_some(),
        "AC1c: api_key.revoked_at must be set after 'cancelled' status patch"
    );
}

// ---------------------------------------------------------------------------
// AC2 — Synchronous cancel (pending session) revokes the API key
// ---------------------------------------------------------------------------

/// `DELETE /v1/agents/sessions/{id}` for a pending session (no PID, no
/// `started_at`) cancels synchronously and revokes the API key.
#[tokio::test]
async fn ac2_sync_cancel_of_pending_session_revokes_api_key() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fix = insert_fixtures(&pool).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/v1/agents/sessions/{}", fix.agent_session_id))
                .header("cookie", format!("rb_session={}", fix.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::ACCEPTED,
        "DELETE should return 202"
    );

    let revoked_at = api_key_revoked_at(&pool, fix.api_key_id).await;
    assert!(
        revoked_at.is_some(),
        "AC2: api_key.revoked_at must be set after synchronous pending-session cancel"
    );
}

// ---------------------------------------------------------------------------
// AC3 — Idempotent revocation
// ---------------------------------------------------------------------------

/// A second PATCH with a terminal status on an already-terminal session
/// does not error and does not clear `revoked_at`.
#[tokio::test]
async fn ac3_double_terminal_patch_is_idempotent() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fix = insert_fixtures(&pool).await;

    let body = json!({
        "status": "failed",
        "tenant_id": fix.tenant_id,
        "error": "first failure"
    });
    let body_bytes = serde_json::to_string(&body).unwrap();

    // First patch — transitions to failed.
    build_internal(state.clone())
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!(
                    "/internal/agent/sessions/{}/status",
                    fix.agent_session_id
                ))
                .header("content-type", "application/json")
                .header("x-internal-secret", INTERNAL_SECRET)
                .body(Body::from(body_bytes.clone()))
                .unwrap(),
        )
        .await
        .unwrap();

    let first_revoked_at = api_key_revoked_at(&pool, fix.api_key_id).await;
    assert!(
        first_revoked_at.is_some(),
        "AC3: first patch must revoke the key"
    );

    // Second patch — the session is already terminal; rows_affected = 0 so no second revoke attempt.
    let resp2 = build_internal(state)
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!(
                    "/internal/agent/sessions/{}/status",
                    fix.agent_session_id
                ))
                .header("content-type", "application/json")
                .header("x-internal-secret", INTERNAL_SECRET)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp2.status(),
        StatusCode::NO_CONTENT,
        "AC3: second patch must not error"
    );

    let second_revoked_at = api_key_revoked_at(&pool, fix.api_key_id).await;
    assert_eq!(
        first_revoked_at, second_revoked_at,
        "AC3: revoked_at must not change on second terminal patch"
    );
}
