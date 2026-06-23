//! Integration tests for the chat-session event-ingest fallback (AC8).
//!
//! Split from `integration_events_ingest_tests.rs` to keep both files under
//! the 600-line cap. Covers the Option-A fix: `events_ingest` falls back to
//! `control.chat_sessions` when the session is absent from `agents.agent_sessions`.
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
// Shared test helpers (duplicated from integration_events_ingest_tests to
// keep each integration-test binary self-contained)
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
        service_name: "control-api-ingest-chat-test".to_owned(),
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
        chat_panel_enabled: false,
        tempo_base_url: "http://localhost:3000".to_owned(),
        mcp_jwt_secret: Some("test-mcp-jwt-secret".to_owned()),
        mcp_jwt_ttl_secs: 900,
        llm_api_key: None,
        hybrid_search_enabled: false,
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
        mcp_jwt_secret: "test-mcp-jwt-secret".to_owned(),
        mcp_jwt_ttl_secs: 900,
        llm_api_key: String::new(),
    };

    Some((state, pool))
}

struct Fixtures {
    tenant_id: Uuid,
    session_id: Uuid,
}

fn ingest_uri(session_id: Uuid) -> String {
    format!("/internal/agent/sessions/{session_id}/events")
}

// ---------------------------------------------------------------------------
// AC8 — chat-session fallback: ingest fans out to SSE without DB insert
//
// Regression for RUSAA-1865: events_ingest used to SELECT from agents.agent_sessions
// only, so chat sessions (stored in control.chat_sessions) produced a 404 and
// starved the SSE bus. Option A fix: fall back to control.chat_sessions lookup.
// ---------------------------------------------------------------------------

/// Insert the minimal tenant → user → `chat_session` fixture rows.
/// Intentionally does NOT insert into `agents.agent_sessions`.
async fn insert_chat_fixture(pool: &PgPool) -> Fixtures {
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();

    let slug = format!("chat-ingest-test-{}", tenant_id.simple());
    let schema_name = format!("chat_ingest_test_{}", tenant_id.simple());

    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(&slug)
    .bind("Chat Ingest Test Tenant")
    .bind(&schema_name)
    .execute(pool)
    .await
    .expect("insert tenant");

    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, email_verified_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(user_id)
    .bind(format!("chat-ingest-{}@test.example", user_id.simple()))
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

    let trace_id = format!("{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO control.chat_sessions \
         (id, tenant_id, user_id, runtime, trace_id) \
         VALUES ($1, $2, $3, 'claude_code', $4)",
    )
    .bind(session_id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(&trace_id)
    .execute(pool)
    .await
    .expect("insert chat_session");

    Fixtures {
        tenant_id,
        session_id,
    }
}

#[tokio::test]
async fn ac8_chat_session_fallback_fans_out_sse_without_db_insert() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_chat_fixture(&pool).await;

    let tenant_id = TenantId::from(fx.tenant_id);
    let (mut rx, _replay): (broadcast::Receiver<Arc<SseEnvelope>>, Vec<Arc<SseEnvelope>>) =
        raw_subscribe(&state.sse_bus, &tenant_id, None);

    let body = json!({
        "tenant_id": fx.tenant_id,
        "events": [
            {"type": "text", "text": "Chat streaming token"}
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
        "AC8: chat ingest must return 200"
    );

    let body_bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let resp_json: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(
        resp_json["inserted"], 1,
        "AC8: must report 1 fanned-out event"
    );

    let envelope = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
        .await
        .expect("AC8: SSE event must arrive within 500 ms")
        .expect("AC8: SSE channel must not be closed");

    assert_eq!(
        envelope.event, "session.event",
        "AC8: SSE event name must be session.event"
    );
    let data: Value =
        serde_json::from_str(&envelope.data).expect("AC8: SSE data must be valid JSON");
    assert_eq!(
        data["event_type"], "session.message",
        "AC8: event_type must be session.message"
    );
    assert_eq!(
        data["session_id"],
        fx.session_id.to_string(),
        "AC8: session_id must match"
    );

    // Chat sessions must NOT produce rows in agents.agent_events.
    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM agents.agent_events WHERE session_id = $1")
            .bind(fx.session_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        count.0, 0,
        "AC8: chat session events must NOT be stored in agents.agent_events"
    );
}

// ---------------------------------------------------------------------------
// AC-parent_user_id — v2 SSE frames carry parent_user_id (spec §4.2, RUSAA-1977)
//
// Verifies that ingest emits parent_user_id on assistant frames and null on
// user_input frames when turn_ids are present in the request.
// ---------------------------------------------------------------------------

/// Insert a user `chat_message` row with a specific `turn_id`, simulating the row
/// that POST /v1/chat/sessions/{id}/messages would have persisted.
async fn insert_user_message(
    pool: &PgPool,
    session_id: Uuid,
    tenant_id: Uuid,
    turn_id: Uuid,
) -> Uuid {
    let user_msg_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO control.chat_messages \
         (id, session_id, tenant_id, seq, role, body, turn_id, parent_user_id) \
         VALUES ($1, $2, $3, 1, 'user', 'hello', $4, NULL)",
    )
    .bind(user_msg_id)
    .bind(session_id)
    .bind(tenant_id)
    .bind(turn_id)
    .execute(pool)
    .await
    .expect("insert user chat_message");
    user_msg_id
}

#[tokio::test]
async fn ac_parent_user_id_present_on_assistant_frames_null_on_user_input() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_chat_fixture(&pool).await;
    let turn_id = Uuid::new_v4();
    // Pre-insert the user message row so the ingest handler can look up parent_user_id.
    let user_msg_id = insert_user_message(&pool, fx.session_id, fx.tenant_id, turn_id).await;

    let tenant_id = TenantId::from(fx.tenant_id);
    let (mut rx, _replay) = raw_subscribe(&state.sse_bus, &tenant_id, None);

    let body = json!({
        "tenant_id": fx.tenant_id,
        "events": [
            {"type": "user_input", "text": "hello"},
            {"type": "text", "text": "world"}
        ],
        "turn_ids": [turn_id.to_string(), turn_id.to_string()]
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

    assert_eq!(resp.status(), StatusCode::OK);

    // Event 1: user_input — parent_user_id must be null per spec.
    let env1 = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
        .await
        .expect("SSE event 1 must arrive")
        .expect("SSE channel open");
    let d1: Value = serde_json::from_str(&env1.data).unwrap();
    assert_eq!(d1["event_type"], "session.user_input");
    assert_eq!(d1["turn_id"], turn_id.to_string());
    assert!(
        d1["parent_user_id"].is_null(),
        "parent_user_id must be null on user_input frame, got {:?}",
        d1["parent_user_id"]
    );

    // Event 2: text — parent_user_id must equal the pre-inserted user message id.
    let env2 = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
        .await
        .expect("SSE event 2 must arrive")
        .expect("SSE channel open");
    let d2: Value = serde_json::from_str(&env2.data).unwrap();
    assert_eq!(d2["event_type"], "session.message");
    assert_eq!(d2["turn_id"], turn_id.to_string());
    assert_eq!(
        d2["parent_user_id"],
        user_msg_id.to_string(),
        "parent_user_id on assistant frame must equal the user message row id"
    );
}

// ---------------------------------------------------------------------------
// RUSAA-2037 — Thinking accumulator: N consecutive Thinking events → ONE block
//
// Before this fix each Thinking event flushed immediately into its own assistant
// row. Now consecutive Thinking payloads within a single batch are merged into
// one `{type:"thinking",thinking:<concatenated>}` block.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rusaa_2037_consecutive_thinking_events_collapse_to_one_block() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_chat_fixture(&pool).await;

    let body = serde_json::json!({
        "tenant_id": fx.tenant_id,
        "events": [
            {"type": "thinking", "thinking": "Step 1: "},
            {"type": "thinking", "thinking": "Step 2: "},
            {"type": "thinking", "thinking": "Step 3."}
        ]
    });

    let resp = build_internal(state)
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(ingest_uri(fx.session_id))
                .header("content-type", "application/json")
                .header("x-internal-secret", INTERNAL_SECRET)
                .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    // Exactly one assistant row must have been persisted.
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT body FROM control.chat_messages \
         WHERE session_id = $1 AND role = 'assistant' \
         ORDER BY seq",
    )
    .bind(fx.session_id)
    .fetch_all(&pool)
    .await
    .expect("query chat_messages");

    assert_eq!(
        rows.len(),
        1,
        "RUSAA-2037: 3 consecutive Thinking events must produce exactly 1 assistant row, got {}",
        rows.len()
    );

    let blocks: Vec<serde_json::Value> =
        serde_json::from_str(&rows[0].0).expect("body must be valid JSON array");

    assert_eq!(
        blocks.len(),
        1,
        "RUSAA-2037: the assistant row must contain exactly 1 content block, got {}",
        blocks.len()
    );

    let block = &blocks[0];
    assert_eq!(
        block["type"], "thinking",
        "RUSAA-2037: block type must be 'thinking', got {:?}",
        block["type"]
    );
    assert_eq!(
        block["thinking"], "Step 1: Step 2: Step 3.",
        "RUSAA-2037: thinking content must be the concatenation of all three chunks"
    );
}
