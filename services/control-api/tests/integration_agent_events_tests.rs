//! Integration tests for `GET /v1/agents/sessions/{id}/events` — SSE endpoint.
//!
//! These tests verify the Server-Sent Events endpoint for agent session streaming.
//! They require a running Postgres instance accessible via `RB_DATABASE_URL`.
//! When that variable is absent, the tests skip gracefully.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use rb_auth::{LoginRateLimiter, PasswordHasher, sha256_hex};
use rb_email::from_transport;
use rb_sse::{EventBus, SseConfig};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt as _;
use uuid::Uuid;

use control_api::{AppState, Config, SessionCreateRateLimiter, TenantSessionCount, build_public};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Build a state connected to a real Postgres instance.
///
/// Returns `None` when `RB_DATABASE_URL` is absent — callers skip gracefully.
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
        service_name: "control-api-agent-events-test".to_owned(),
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
    };
    Some((state, pool))
}

/// Fixture result: everything the caller needs to drive the agent session events endpoint.
struct AgentSessionFixtures {
    session_token: String,
    session_id: Uuid,
    #[allow(dead_code)]
    tenant_id: Uuid,
    #[allow(dead_code)]
    user_id: Uuid,
}

/// Insert the minimal set of control and agent schema rows required:
/// tenant → user (email-verified) → session → `agent_session`.
///
/// All rows use fresh UUIDs so parallel test runs never collide.
async fn insert_agent_session_fixtures(pool: &PgPool) -> AgentSessionFixtures {
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let agent_session_id = Uuid::new_v4();

    let slug = format!("agent-events-test-{}", tenant_id.simple());
    let schema_name = format!("agent_events_{}", tenant_id.simple());

    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(&slug)
    .bind("Agent Events Integration Tenant")
    .bind(&schema_name)
    .execute(pool)
    .await
    .expect("insert tenant");

    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, email_verified_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(user_id)
    .bind(format!("agent-events-{}@test.example", user_id.simple()))
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

    let session_token = format!("agent-events-test-token-{}", Uuid::new_v4().simple());
    let token_hash = sha256_hex(&session_token);
    sqlx::query(
        "INSERT INTO control.sessions (id, user_id, tenant_id, token_hash, expires_at) \
         VALUES ($1, $2, $3, $4, now() + interval '30 days')",
    )
    .bind(session_id)
    .bind(user_id)
    .bind(tenant_id)
    .bind(&token_hash)
    .execute(pool)
    .await
    .expect("insert session");

    // Insert agent session row (required for SSE endpoint lookup)
    sqlx::query(
        "INSERT INTO agents.agent_sessions \
         (id, tenant_id, user_id, runtime_kind, model, system_prompt, status, \
          token_budget, tokens_used, input_prompt_preview, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now())",
    )
    .bind(agent_session_id)
    .bind(tenant_id)
    .bind(user_id)
    .bind("opencode")
    .bind("gpt-4o-mini")
    .bind("You are a helpful assistant")
    .bind("created")
    .bind(100_000i64)
    .bind(0i64)
    .bind("test prompt preview")
    .execute(pool)
    .await
    .expect("insert agent_session");

    AgentSessionFixtures {
        session_token,
        session_id: agent_session_id,
        tenant_id,
        user_id,
    }
}

// ---------------------------------------------------------------------------
// AC1 — 401 Unauthorized without session cookie
// ---------------------------------------------------------------------------

/// AC1: `GET /v1/agents/sessions/{id}/events` must return **401 Unauthorized**
/// when no session cookie is provided.
#[tokio::test]
async fn ac1_events_stream_without_session_returns_401() {
    let Some((state, _pool)) = real_db_state().await else {
        return; // skip: no DB
    };

    let session_id = Uuid::new_v4();
    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1/agents/sessions/{session_id}/events"))
                .header("accept", "text/event-stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "AC1: no session cookie must yield 401, got {}",
        resp.status()
    );
}

// ---------------------------------------------------------------------------
// AC2 — 200 OK with SSE content type for valid session owner
// ---------------------------------------------------------------------------

/// AC2: `GET /v1/agents/sessions/{id}/events` must return **200 OK** with
/// `Content-Type: text/event-stream` for the session owner.
#[tokio::test]
async fn ac2_events_stream_returns_sse_for_session_owner() {
    let Some((state, pool)) = real_db_state().await else {
        return; // skip: no DB
    };

    let fixtures = insert_agent_session_fixtures(&pool).await;

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/v1/agents/sessions/{}/events",
                    fixtures.session_id
                ))
                .header("accept", "text/event-stream")
                .header("cookie", format!("rb_session={}", fixtures.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "AC2: session owner must get 200, got {}",
        resp.status()
    );

    let content_type = resp
        .headers()
        .get("content-type")
        .expect("content-type header must be present")
        .to_str()
        .unwrap();
    assert!(
        content_type.contains("text/event-stream"),
        "AC2: content-type must be text/event-stream, got {content_type}"
    );
}

// ---------------------------------------------------------------------------
// AC3 — 404 Not Found for non-existent session
// ---------------------------------------------------------------------------

/// AC3: `GET /v1/agents/sessions/{id}/events` must return **404 Not Found**
/// when the session ID does not exist in the database.
#[tokio::test]
async fn ac3_events_stream_nonexistent_session_returns_404() {
    let Some((state, pool)) = real_db_state().await else {
        return; // skip: no DB
    };

    let fixtures = insert_agent_session_fixtures(&pool).await;
    let nonexistent_session_id = Uuid::new_v4();

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/v1/agents/sessions/{nonexistent_session_id}/events"
                ))
                .header("accept", "text/event-stream")
                .header("cookie", format!("rb_session={}", fixtures.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "AC3: nonexistent session must yield 404, got {}",
        resp.status()
    );
}

// ---------------------------------------------------------------------------
// AC4 — 403 Forbidden when accessing another tenant's session
// ---------------------------------------------------------------------------

/// AC4: `GET /v1/agents/sessions/{id}/events` must return **403 Forbidden**
/// when the caller attempts to access a session belonging to a different tenant.
#[tokio::test]
async fn ac4_events_stream_cross_tenant_access_denied() {
    let Some((state, pool)) = real_db_state().await else {
        return; // skip: no DB
    };

    // Create first user's session
    let fixtures_a = insert_agent_session_fixtures(&pool).await;

    // Create second user in different tenant with their own session
    let tenant_b_id = Uuid::new_v4();
    let user_b_id = Uuid::new_v4();
    let session_b_id = Uuid::new_v4();
    let agent_session_b_id = Uuid::new_v4();

    let slug_b = format!("tenant-b-{}", tenant_b_id.simple());
    let schema_b = format!("tenant_b_{}", tenant_b_id.simple());

    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_b_id)
    .bind(&slug_b)
    .bind("Tenant B")
    .bind(&schema_b)
    .execute(&pool)
    .await
    .expect("insert tenant B");

    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, email_verified_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(user_b_id)
    .bind(format!("user-b-{}@test.example", user_b_id.simple()))
    .bind("$argon2id$v=19$m=65536,t=1,p=1$placeholder_hash")
    .execute(&pool)
    .await
    .expect("insert user B");

    sqlx::query(
        "INSERT INTO control.tenant_members (tenant_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(tenant_b_id)
    .bind(user_b_id)
    .execute(&pool)
    .await
    .expect("insert tenant_member B");

    let session_token_b = format!("session-b-token-{}", Uuid::new_v4().simple());
    let token_hash_b = sha256_hex(&session_token_b);
    sqlx::query(
        "INSERT INTO control.sessions (id, user_id, tenant_id, token_hash, expires_at) \
         VALUES ($1, $2, $3, $4, now() + interval '30 days')",
    )
    .bind(session_b_id)
    .bind(user_b_id)
    .bind(tenant_b_id)
    .bind(&token_hash_b)
    .execute(&pool)
    .await
    .expect("insert session B");

    sqlx::query(
        "INSERT INTO agents.agent_sessions \
         (id, tenant_id, user_id, runtime_kind, model, system_prompt, status, \
          token_budget, tokens_used, input_prompt_preview, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now())",
    )
    .bind(agent_session_b_id)
    .bind(tenant_b_id)
    .bind(user_b_id)
    .bind("opencode")
    .bind("gpt-4o-mini")
    .bind("You are a helpful assistant")
    .bind("created")
    .bind(100_000i64)
    .bind(0i64)
    .bind("test prompt preview B")
    .execute(&pool)
    .await
    .expect("insert agent_session B");

    // User A tries to access User B's session
    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1/agents/sessions/{agent_session_b_id}/events"))
                .header("accept", "text/event-stream")
                .header("cookie", format!("rb_session={}", fixtures_a.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "AC4: cross-tenant access must yield 403, got {}",
        resp.status()
    );
}
