//! Integration tests for `POST /internal/agent/sessions/{id}/events` — bulk ingest endpoint.
//!
//! Verifies: happy-path bulk insert, tenant mismatch (401), unknown session (404),
//! missing internal secret (401), and SSE fan-out after commit.
//!
//! Requires a running Postgres instance accessible via `RB_DATABASE_URL`.
//! Tests skip gracefully when that variable is absent.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use rb_auth::{LoginRateLimiter, PasswordHasher};
use rb_email::from_transport;
use rb_schemas::TenantId;
use rb_sse::{EventBus, SseConfig, SseEnvelope, testing::raw_subscribe};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::sync::broadcast;
use tower::ServiceExt as _;
use uuid::Uuid;

use control_api::{
    AppState, Config, KafkaConsistencyState, SessionCreateRateLimiter, TenantSessionCount,
    build_internal,
};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

const INTERNAL_SECRET: &str = "test-ingest-internal-secret";

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
        service_name: "control-api-ingest-test".to_owned(),
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
        kafka_consistency: Arc::new(KafkaConsistencyState::new()),
        mcp_sessions: control_api::McpSessionStore::new(),
        agent_registry: control_api::AgentRegistry::new(),
        agent_commands_producer: None,
        internal_secret: INTERNAL_SECRET.to_owned(),
        session_create_rate_limiter: Arc::new(SessionCreateRateLimiter::default()),
        tenant_session_count: Arc::new(TenantSessionCount::new()),
    };

    Some((state, pool))
}

struct Fixtures {
    tenant_id: Uuid,
    session_id: Uuid,
}

/// Insert the minimal tenant → user → `agent_session` fixture rows.
async fn insert_fixtures(pool: &PgPool) -> Fixtures {
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();

    let slug = format!("ingest-test-{}", tenant_id.simple());
    let schema_name = format!("ingest_test_{}", tenant_id.simple());

    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(&slug)
    .bind("Ingest Test Tenant")
    .bind(&schema_name)
    .execute(pool)
    .await
    .expect("insert tenant");

    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, email_verified_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(user_id)
    .bind(format!("ingest-{}@test.example", user_id.simple()))
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

    sqlx::query(
        "INSERT INTO agents.agent_sessions \
         (id, tenant_id, user_id, runtime_kind, model, system_prompt, status, \
          token_budget, tokens_used, input_prompt_preview, workspace_path, created_at) \
         VALUES ($1, $2, $3, 'claude_code', 'claude-sonnet-4-5', '', 'running', \
                 100000, 0, 'test prompt', '', now())",
    )
    .bind(session_id)
    .bind(tenant_id)
    .bind(user_id)
    .execute(pool)
    .await
    .expect("insert agent_session");

    Fixtures {
        tenant_id,
        session_id,
    }
}

fn ingest_uri(session_id: Uuid) -> String {
    format!("/internal/agent/sessions/{session_id}/events")
}

// ---------------------------------------------------------------------------
// AC1 — happy path: bulk insert succeeds and sequence numbers are assigned
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac1_bulk_insert_happy_path() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;

    let body = json!({
        "tenant_id": fx.tenant_id,
        "events": [
            {"type": "text", "text": "Hello, world!"},
            {"type": "thinking", "thinking": "Let me think..."},
            {"type": "tool_use", "id": "toolu_01", "name": "read_file", "input": {"path": "src/main.rs"}}
        ]
    });

    let resp = build_internal(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(ingest_uri(fx.session_id))
                .header("content-type", "application/json")
                .header("x-internal-secret", INTERNAL_SECRET)
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "AC1: happy path must return 200"
    );

    let body_bytes = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let resp_json: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(
        resp_json["inserted"], 3,
        "AC1: must report 3 inserted events"
    );

    // Verify rows exist in the DB with sequential sequence values.
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT event_type, sequence FROM agents.agent_events WHERE session_id = $1 AND sequence >= 0 ORDER BY sequence ASC")
            .bind(fx.session_id)
            .fetch_all(&pool)
            .await
            .unwrap();

    assert_eq!(rows.len(), 3, "AC1: 3 rows must be in agent_events");
    assert_eq!(rows[0].0, "session.message");
    assert_eq!(rows[1].0, "session.thinking");
    assert_eq!(rows[2].0, "session.tool_call");
    // Sequences must be strictly increasing.
    assert!(
        rows[0].1 < rows[1].1 && rows[1].1 < rows[2].1,
        "AC1: sequences must be monotonically increasing"
    );
}

// ---------------------------------------------------------------------------
// AC2 — tenant mismatch returns 401
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac2_tenant_mismatch_returns_401() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;
    let wrong_tenant = Uuid::new_v4();

    let body = json!({
        "tenant_id": wrong_tenant,
        "events": [{"type": "text", "text": "hi"}]
    });

    let resp = build_internal(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(ingest_uri(fx.session_id))
                .header("content-type", "application/json")
                .header("x-internal-secret", INTERNAL_SECRET)
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "AC2: tenant mismatch must return 401"
    );
}

// ---------------------------------------------------------------------------
// AC3 — unknown session returns 404
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac3_unknown_session_returns_404() {
    let Some((state, _pool)) = real_db_state().await else {
        return;
    };
    let nonexistent = Uuid::new_v4();
    let some_tenant = Uuid::new_v4();

    let body = json!({
        "tenant_id": some_tenant,
        "events": [{"type": "text", "text": "hi"}]
    });

    let resp = build_internal(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(ingest_uri(nonexistent))
                .header("content-type", "application/json")
                .header("x-internal-secret", INTERNAL_SECRET)
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "AC3: unknown session must return 404"
    );
}

// ---------------------------------------------------------------------------
// AC4 — missing internal secret returns 401
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac4_missing_internal_secret_returns_401() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;

    let body = json!({
        "tenant_id": fx.tenant_id,
        "events": [{"type": "text", "text": "hi"}]
    });

    let resp = build_internal(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(ingest_uri(fx.session_id))
                .header("content-type", "application/json")
                // intentionally omit x-internal-secret
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "AC4: missing internal secret must return 401"
    );
}

// ---------------------------------------------------------------------------
// AC5 — empty event batch returns 200 with inserted = 0
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac5_empty_events_returns_ok_with_zero() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;

    let body = json!({
        "tenant_id": fx.tenant_id,
        "events": []
    });

    let resp = build_internal(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(ingest_uri(fx.session_id))
                .header("content-type", "application/json")
                .header("x-internal-secret", INTERNAL_SECRET)
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "AC5: empty batch must return 200"
    );
    let body_bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let resp_json: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(resp_json["inserted"], 0, "AC5: inserted must be 0");
}

// ---------------------------------------------------------------------------
// AC6 — sequences continue from existing max when called twice
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac6_sequences_continue_from_previous_batch() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;

    // First batch: 2 events.
    let body1 = json!({
        "tenant_id": fx.tenant_id,
        "events": [
            {"type": "text", "text": "first"},
            {"type": "text", "text": "second"}
        ]
    });
    let resp1 = build_internal(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(ingest_uri(fx.session_id))
                .header("content-type", "application/json")
                .header("x-internal-secret", INTERNAL_SECRET)
                .body(Body::from(serde_json::to_vec(&body1).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::OK);

    // Second batch: 1 event.
    let body2 = json!({
        "tenant_id": fx.tenant_id,
        "events": [{"type": "error", "message": "boom"}]
    });
    let resp2 = build_internal(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(ingest_uri(fx.session_id))
                .header("content-type", "application/json")
                .header("x-internal-secret", INTERNAL_SECRET)
                .body(Body::from(serde_json::to_vec(&body2).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);

    // Verify the 3 rows have monotonically increasing sequences.
    let rows: Vec<(i64,)> =
        sqlx::query_as("SELECT sequence FROM agents.agent_events WHERE session_id = $1 AND sequence >= 0 ORDER BY sequence ASC")
            .bind(fx.session_id)
            .fetch_all(&pool)
            .await
            .unwrap();

    assert_eq!(rows.len(), 3, "AC6: 3 rows total after two batches");
    let seqs: Vec<i64> = rows.into_iter().map(|(s,)| s).collect();
    let is_strictly_increasing = seqs.windows(2).all(|w| w[0] < w[1]);
    assert!(
        is_strictly_increasing,
        "AC6: sequences must be strictly increasing: {seqs:?}"
    );
    // Second batch's event must have sequence > any from the first batch.
    assert!(seqs[2] > seqs[1], "AC6: third event must follow second");
}

// ---------------------------------------------------------------------------
// AC7 — SSE fan-out: published events reach a subscriber
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac7_sse_fanout_reaches_subscriber() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;

    let tenant_id = TenantId::from(fx.tenant_id);
    let (mut rx, _replay): (broadcast::Receiver<Arc<SseEnvelope>>, Vec<Arc<SseEnvelope>>) =
        raw_subscribe(&state.sse_bus, &tenant_id, None);

    let body = json!({
        "tenant_id": fx.tenant_id,
        "events": [
            {"type": "text", "text": "SSE test message"}
        ]
    });

    let resp = build_internal(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(ingest_uri(fx.session_id))
                .header("content-type", "application/json")
                .header("x-internal-secret", INTERNAL_SECRET)
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK, "AC7: ingest must succeed");

    // The SSE envelope must arrive within 500 ms.
    let envelope = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
        .await
        .expect("AC7: SSE event must arrive within 500 ms")
        .expect("AC7: SSE channel must not be closed");

    assert_eq!(
        envelope.event, "session.event",
        "AC7: SSE event name must be session.event"
    );
    let data: Value =
        serde_json::from_str(&envelope.data).expect("AC7: SSE data must be valid JSON");
    assert_eq!(
        data["event_type"], "session.message",
        "AC7: event_type must be session.message"
    );
    assert_eq!(
        data["session_id"],
        fx.session_id.to_string(),
        "AC7: session_id must match"
    );
}
