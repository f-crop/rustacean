//! Integration tests for `GET /v1/agents/sessions/{id}/events/history` — paged history (RUSAA-1317).
//!
//! Covers: 401 no-auth, 404 unknown session, 403 cross-tenant, empty result,
//! pagination correctness, boundary conditions (`after` at start / past end),
//! default-limit enforcement, and invalid-limit rejection.
//!
//! Requires a running Postgres instance via `RB_DATABASE_URL`.  Tests skip
//! gracefully when that variable is absent.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use rb_auth::{LoginRateLimiter, PasswordHasher, sha256_hex};
use rb_email::from_transport;
use rb_sse::{EventBus, SseConfig};
use serde_json::Value;
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt as _;
use uuid::Uuid;

use control_api::{
    AppState, Config, KafkaConsistencyState, SessionCreateRateLimiter, TenantSessionCount,
    build_public,
};

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
        service_name: "control-api-history-test".to_owned(),
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
        internal_secret: Some("test-history-internal-secret".to_owned()),
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
        internal_secret: "test-history-internal-secret".to_owned(),
        session_create_rate_limiter: Arc::new(SessionCreateRateLimiter::default()),
        tenant_session_count: Arc::new(TenantSessionCount::new()),
    };

    Some((state, pool))
}

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

struct Fixtures {
    session_token: String,
    agent_session_id: Uuid,
    tenant_id: Uuid,
    #[allow(dead_code)]
    user_id: Uuid,
}

async fn insert_fixtures(pool: &PgPool) -> Fixtures {
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let web_session_id = Uuid::new_v4();
    let agent_session_id = Uuid::new_v4();

    let slug = format!("history-test-{}", tenant_id.simple());
    let schema_name = format!("history_test_{}", tenant_id.simple());

    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(&slug)
    .bind("History Test Tenant")
    .bind(&schema_name)
    .execute(pool)
    .await
    .expect("insert tenant");

    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, email_verified_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(user_id)
    .bind(format!("history-{}@test.example", user_id.simple()))
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

    let session_token = format!("history-test-token-{}", Uuid::new_v4().simple());
    let token_hash = sha256_hex(&session_token);
    sqlx::query(
        "INSERT INTO control.sessions (id, user_id, tenant_id, token_hash, expires_at) \
         VALUES ($1, $2, $3, $4, now() + interval '30 days')",
    )
    .bind(web_session_id)
    .bind(user_id)
    .bind(tenant_id)
    .bind(&token_hash)
    .execute(pool)
    .await
    .expect("insert web session");

    sqlx::query(
        "INSERT INTO agents.agent_sessions \
         (id, tenant_id, user_id, runtime_kind, model, system_prompt, status, \
          token_budget, tokens_used, input_prompt_preview, created_at) \
         VALUES ($1, $2, $3, 'claude_code', 'claude-sonnet-4-5', '', 'completed', \
                 100000, 0, 'history test', now())",
    )
    .bind(agent_session_id)
    .bind(tenant_id)
    .bind(user_id)
    .execute(pool)
    .await
    .expect("insert agent_session");

    Fixtures {
        session_token,
        agent_session_id,
        tenant_id,
        user_id,
    }
}

/// Insert `n` events into `agents.agent_events` with sequences 1..=n.
async fn insert_n_events(pool: &PgPool, session_id: Uuid, tenant_id: Uuid, n: usize) {
    for i in 1..=n {
        let payload = serde_json::json!({ "text": format!("event {i}") });
        let seq = i64::try_from(i).expect("event index fits i64");
        sqlx::query(
            "INSERT INTO agents.agent_events \
             (session_id, tenant_id, event_type, sequence, payload) \
             VALUES ($1, $2, 'session.message', $3, $4)",
        )
        .bind(session_id)
        .bind(tenant_id)
        .bind(seq)
        .bind(&payload)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("insert event {i}: {e}"));
    }
}

fn history_uri(session_id: Uuid) -> String {
    format!("/v1/agents/sessions/{session_id}/events/history")
}

fn history_uri_with_params(session_id: Uuid, after: Option<i64>, limit: Option<i64>) -> String {
    let mut uri = history_uri(session_id);
    let mut parts: Vec<String> = vec![];
    if let Some(a) = after {
        parts.push(format!("after={a}"));
    }
    if let Some(l) = limit {
        parts.push(format!("limit={l}"));
    }
    if !parts.is_empty() {
        uri.push('?');
        uri.push_str(&parts.join("&"));
    }
    uri
}

// ---------------------------------------------------------------------------
// AC1 — 401 when no auth header / cookie
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac1_no_auth_returns_401() {
    let Some((state, _pool)) = real_db_state().await else {
        return;
    };
    let session_id = Uuid::new_v4();
    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri(session_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "AC1: must be 401");
}

// ---------------------------------------------------------------------------
// AC2 — 404 for unknown session
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac2_unknown_session_returns_404() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri(Uuid::new_v4()))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "AC2: must be 404");
}

// ---------------------------------------------------------------------------
// AC3 — 403 cross-tenant access
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac3_cross_tenant_returns_403() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx_a = insert_fixtures(&pool).await;
    let fx_b = insert_fixtures(&pool).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri(fx_a.agent_session_id))
                .header("cookie", format!("rb_session={}", fx_b.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "AC3: must be 403");
}

// ---------------------------------------------------------------------------
// AC4 — empty result when session has no events
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac4_empty_session_returns_200_with_empty_events() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri(fx.agent_session_id))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK, "AC4: must be 200");

    let bytes = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).expect("AC4: must be valid JSON");

    assert!(body["events"].is_array(), "AC4: events must be an array");
    assert_eq!(
        body["events"].as_array().unwrap().len(),
        0,
        "AC4: events must be empty"
    );
    assert!(body["next_seq"].is_null(), "AC4: next_seq must be null");
}

// ---------------------------------------------------------------------------
// AC5 — pagination correctness: two pages from 150 events with limit=100
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac5_pagination_two_pages_from_150_events() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;
    insert_n_events(&pool, fx.agent_session_id, fx.tenant_id, 150).await;

    // Page 1: no `after`, limit=100
    let resp1 = build_public(state.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri_with_params(
                    fx.agent_session_id,
                    None,
                    Some(100),
                ))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp1.status(), StatusCode::OK, "AC5-p1: must be 200");
    let body1: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp1.into_body(), 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();

    let events1 = body1["events"].as_array().unwrap();
    assert_eq!(
        events1.len(),
        100,
        "AC5-p1: first page must have 100 events"
    );

    // next_seq must be 100 (sequence of the last event on page 1).
    let next_seq = body1["next_seq"]
        .as_i64()
        .expect("AC5-p1: next_seq must be present");
    assert_eq!(next_seq, 100, "AC5-p1: next_seq must be 100");

    // Sequences must be 1..=100 in order.
    for (i, ev) in events1.iter().enumerate() {
        let seq = ev["sequence"].as_i64().unwrap();
        assert_eq!(
            seq,
            i64::try_from(i + 1).unwrap(),
            "AC5-p1: sequence at position {i} must be {}",
            i + 1
        );
    }

    // Page 2: after=100, limit=100
    let resp2 = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri_with_params(
                    fx.agent_session_id,
                    Some(next_seq),
                    Some(100),
                ))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp2.status(), StatusCode::OK, "AC5-p2: must be 200");
    let body2: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp2.into_body(), 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();

    let events2 = body2["events"].as_array().unwrap();
    assert_eq!(events2.len(), 50, "AC5-p2: second page must have 50 events");
    assert!(
        body2["next_seq"].is_null(),
        "AC5-p2: next_seq must be null on last page"
    );

    // Sequences must be 101..=150 in order.
    for (i, ev) in events2.iter().enumerate() {
        let seq = ev["sequence"].as_i64().unwrap();
        assert_eq!(
            seq,
            i64::try_from(i + 101).unwrap(),
            "AC5-p2: sequence at position {i} must be {}",
            i + 101
        );
    }
}

// ---------------------------------------------------------------------------
// AC6 — `after` at the beginning: same as no `after`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac6_after_zero_equivalent_to_no_after() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;
    insert_n_events(&pool, fx.agent_session_id, fx.tenant_id, 10).await;

    // Sequences start at 1, so after=0 is effectively the same as no `after`.
    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri_with_params(fx.agent_session_id, Some(0), None))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK, "AC6: must be 200");
    let body: Value =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 4096).await.unwrap())
            .unwrap();

    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 10, "AC6: must return all 10 events");
    assert!(body["next_seq"].is_null(), "AC6: next_seq must be null");
}

// ---------------------------------------------------------------------------
// AC7 — `after` past last sequence returns empty page
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac7_after_past_last_seq_returns_empty() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;
    insert_n_events(&pool, fx.agent_session_id, fx.tenant_id, 5).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri_with_params(
                    fx.agent_session_id,
                    Some(9999),
                    None,
                ))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK, "AC7: must be 200");
    let body: Value =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 4096).await.unwrap())
            .unwrap();

    assert_eq!(
        body["events"].as_array().unwrap().len(),
        0,
        "AC7: must return empty events array"
    );
    assert!(body["next_seq"].is_null(), "AC7: next_seq must be null");
}

// ---------------------------------------------------------------------------
// AC8 — default limit is 100: insert 200 events, no explicit limit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac8_default_limit_is_100() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;
    insert_n_events(&pool, fx.agent_session_id, fx.tenant_id, 200).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri(fx.agent_session_id))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK, "AC8: must be 200");
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();

    let events = body["events"].as_array().unwrap();
    assert_eq!(
        events.len(),
        100,
        "AC8: default limit must return exactly 100 events"
    );
    assert!(
        body["next_seq"].as_i64().is_some(),
        "AC8: next_seq must be present when more events exist"
    );
}

// ---------------------------------------------------------------------------
// AC9 — invalid limit returns 400
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac9_limit_zero_returns_400() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;

    let resp = build_public(state.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri_with_params(fx.agent_session_id, None, Some(0)))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "AC9: limit=0 must return 400"
    );
}

#[tokio::test]
async fn ac9_limit_above_max_returns_400() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri_with_params(
                    fx.agent_session_id,
                    None,
                    Some(501),
                ))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "AC9: limit=501 must return 400"
    );
}

// ---------------------------------------------------------------------------
// AC10 — response shape: events have expected fields
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac10_response_shape_has_expected_fields() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;
    insert_n_events(&pool, fx.agent_session_id, fx.tenant_id, 3).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(history_uri(fx.agent_session_id))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK, "AC10: must be 200");
    let body: Value =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 4096).await.unwrap())
            .unwrap();

    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 3, "AC10: must return 3 events");

    let ev = &events[0];
    assert!(ev.get("id").is_some(), "AC10: event must have id");
    assert!(
        ev.get("session_id").is_some(),
        "AC10: event must have session_id"
    );
    assert!(
        ev.get("tenant_id").is_some(),
        "AC10: event must have tenant_id"
    );
    assert!(
        ev.get("event_type").is_some(),
        "AC10: event must have event_type"
    );
    assert!(
        ev.get("sequence").is_some(),
        "AC10: event must have sequence"
    );
    assert!(ev.get("payload").is_some(), "AC10: event must have payload");
    assert!(
        ev.get("created_at").is_some(),
        "AC10: event must have created_at"
    );
}
