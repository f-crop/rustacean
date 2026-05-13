//! Integration tests for `GET /v1/agents/sessions/{id}/log.ndjson` — NDJSON download.
//!
//! Verifies: happy-path stream with correct headers, 1000-event ordering test,
//! tenant isolation, missing-auth 401, unknown-session 404, and raw-scope guard.
//!
//! Requires a running Postgres instance accessible via `RB_DATABASE_URL`.
//! Tests skip gracefully when that variable is absent.

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
// State builder (shared across tests)
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
        service_name: "control-api-ndjson-test".to_owned(),
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
        internal_secret: Some("test-ndjson-internal-secret".to_owned()),
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
        internal_secret: "test-ndjson-internal-secret".to_owned(),
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

    let slug = format!("ndjson-test-{}", tenant_id.simple());
    let schema_name = format!("ndjson_test_{}", tenant_id.simple());

    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(&slug)
    .bind("NDJSON Test Tenant")
    .bind(&schema_name)
    .execute(pool)
    .await
    .expect("insert tenant");

    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, email_verified_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(user_id)
    .bind(format!("ndjson-{}@test.example", user_id.simple()))
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

    let session_token = format!("ndjson-test-token-{}", Uuid::new_v4().simple());
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
                 100000, 0, 'ndjson test', now())",
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

/// Insert `n` events directly into `agents.agent_events` for the given session.
/// Sequences start at 1 and increment by 1.
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

fn ndjson_uri(session_id: Uuid) -> String {
    format!("/v1/agents/sessions/{session_id}/log.ndjson")
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
                .uri(ndjson_uri(session_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "AC1: missing auth must return 401"
    );
}

// ---------------------------------------------------------------------------
// AC2 — correct headers for a valid owner request
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac2_valid_owner_returns_ndjson_headers() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(ndjson_uri(fx.agent_session_id))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "AC2: session owner must get 200"
    );

    let ct = resp
        .headers()
        .get("content-type")
        .expect("content-type header must be present")
        .to_str()
        .unwrap();
    assert!(
        ct.contains("application/x-ndjson"),
        "AC2: content-type must be application/x-ndjson, got {ct}"
    );

    let cd = resp
        .headers()
        .get("content-disposition")
        .expect("content-disposition header must be present")
        .to_str()
        .unwrap();
    assert!(
        cd.contains(&fx.agent_session_id.to_string()),
        "AC2: content-disposition must contain session id, got {cd}"
    );
    assert!(
        cd.starts_with("attachment;"),
        "AC2: content-disposition must be attachment, got {cd}"
    );
}

// ---------------------------------------------------------------------------
// AC3 — 1000-event stream: parseable NDJSON, correct count and ordering
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac3_streams_1000_events_ordered_ndjson() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;
    insert_n_events(&pool, fx.agent_session_id, fx.tenant_id, 1000).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(ndjson_uri(fx.agent_session_id))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK, "AC3: must return 200");

    // Consume the full streaming body.
    let bytes = axum::body::to_bytes(resp.into_body(), 10 * 1024 * 1024)
        .await
        .expect("AC3: body must not exceed 10 MiB");
    let body_str = std::str::from_utf8(&bytes).expect("AC3: body must be valid UTF-8");

    // Each line is a JSON object.
    let lines: Vec<&str> = body_str.lines().filter(|l| !l.trim().is_empty()).collect();

    assert_eq!(lines.len(), 1000, "AC3: must stream exactly 1000 lines");

    // Parse each line and verify sequence ordering.
    let mut last_seq: i64 = i64::MIN;
    for (i, line) in lines.iter().enumerate() {
        let obj: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("AC3: line {i} not valid JSON: {e}\n  content: {line}"));

        let seq = obj["sequence"]
            .as_i64()
            .unwrap_or_else(|| panic!("AC3: line {i} missing 'sequence' field"));

        assert!(
            seq > last_seq,
            "AC3: sequence must be strictly increasing (line {i}: {seq} <= {last_seq})"
        );
        last_seq = seq;

        assert_eq!(
            obj["event_type"], "session.message",
            "AC3: all events must be session.message"
        );
    }
}

// ---------------------------------------------------------------------------
// AC4 — 404 for unknown session
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac4_unknown_session_returns_404() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;
    let nonexistent = Uuid::new_v4();

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(ndjson_uri(nonexistent))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "AC4: unknown session must return 404"
    );
}

// ---------------------------------------------------------------------------
// AC5 — 403 when a different tenant's user requests the session
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac5_cross_tenant_returns_403() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };

    let fx_a = insert_fixtures(&pool).await;
    let fx_b = insert_fixtures(&pool).await;

    // User B tries to download User A's session.
    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(ndjson_uri(fx_a.agent_session_id))
                .header("cookie", format!("rb_session={}", fx_b.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "AC5: cross-tenant access must return 403"
    );
}

// ---------------------------------------------------------------------------
// AC6 — empty stream when session has no events
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac6_empty_session_returns_200_with_empty_body() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(ndjson_uri(fx.agent_session_id))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "AC6: empty session must return 200"
    );

    let bytes = axum::body::to_bytes(resp.into_body(), 4096)
        .await
        .expect("AC6: body read failed");
    let body_str = std::str::from_utf8(&bytes).expect("AC6: body must be UTF-8");
    let non_empty_lines = body_str.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(
        non_empty_lines, 0,
        "AC6: empty session must produce no NDJSON lines"
    );
}

// ---------------------------------------------------------------------------
// AC7 — ?raw=1 without admin scope returns 403
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac7_raw_without_admin_scope_returns_403() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };
    let fx = insert_fixtures(&pool).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/v1/agents/sessions/{}/log.ndjson?raw=1",
                    fx.agent_session_id
                ))
                // Session-based auth has no "admin" scope concept — same as non-admin.
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "AC7: ?raw=1 without admin scope must return 403"
    );
}

// ---------------------------------------------------------------------------
// AC8 — owner accessing their non-owner session returns 403
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac8_non_owner_same_tenant_returns_403() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };

    let fx_a = insert_fixtures(&pool).await;

    // Insert a second user in the same tenant as fx_a.
    let user_b_id = Uuid::new_v4();
    let web_session_b_id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, email_verified_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(user_b_id)
    .bind(format!("ndjson-b-{}@test.example", user_b_id.simple()))
    .bind("$argon2id$v=19$m=65536,t=1,p=1$placeholder_hash")
    .execute(&pool)
    .await
    .expect("insert user B");

    sqlx::query(
        "INSERT INTO control.tenant_members (tenant_id, user_id, role) VALUES ($1, $2, 'member')",
    )
    .bind(fx_a.tenant_id)
    .bind(user_b_id)
    .execute(&pool)
    .await
    .expect("insert tenant_member B");

    let token_b = format!("ndjson-b-token-{}", Uuid::new_v4().simple());
    let hash_b = sha256_hex(&token_b);
    sqlx::query(
        "INSERT INTO control.sessions (id, user_id, tenant_id, token_hash, expires_at) \
         VALUES ($1, $2, $3, $4, now() + interval '30 days')",
    )
    .bind(web_session_b_id)
    .bind(user_b_id)
    .bind(fx_a.tenant_id)
    .bind(&hash_b)
    .execute(&pool)
    .await
    .expect("insert web session B");

    // User B (same tenant, different user) tries to download User A's session.
    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(ndjson_uri(fx_a.agent_session_id))
                .header("cookie", format!("rb_session={token_b}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "AC8: non-owner same-tenant access must return 403"
    );
}
