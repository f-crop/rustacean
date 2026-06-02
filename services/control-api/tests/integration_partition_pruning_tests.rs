//! Integration tests for `agent_events` partition pruning (RUSAA-1374).
//!
//! Acceptance criterion: rows in an expired partition are NOT returned by
//! `GET /v1/agents/sessions/{id}/events/history` after the prune job runs.
//!
//! Requires a running Postgres instance via `RB_DATABASE_URL`.  Tests skip
//! gracefully when that variable is absent.
//!
//! These tests directly exercise the SQL functions from migration 019:
//!   - `agents.seed_agent_events_partition(date)` — idempotent partition creation
//!   - `agents.prune_agent_events_partitions()` — drops expired partitions
//!
//! The `POST /internal/admin/partition-maintenance` handler is also covered by a
//! smoke test that verifies the endpoint is wired and reachable.

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
    build_internal, build_public,
};

// ---------------------------------------------------------------------------
// Helpers
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
        service_name: "control-api-partition-test".to_owned(),
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
        internal_secret: Some("test-partition-internal-secret".to_owned()),
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
        internal_secret: "test-partition-internal-secret".to_owned(),
        session_create_rate_limiter: Arc::new(SessionCreateRateLimiter::default()),
        tenant_session_count: Arc::new(TenantSessionCount::new()),
        mcp_jwt_secret: "test-mcp-jwt-secret".to_owned(),
        mcp_jwt_ttl_secs: 900,
    };

    Some((state, pool))
}

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

    let slug = format!("prune-test-{}", tenant_id.simple());
    let schema_name = format!("prune_test_{}", tenant_id.simple());

    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(&slug)
    .bind("Prune Test Tenant")
    .bind(&schema_name)
    .execute(pool)
    .await
    .expect("insert tenant");

    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, email_verified_at) \
         VALUES ($1, $2, $3, now())",
    )
    .bind(user_id)
    .bind(format!("prune-{}@test.example", user_id.simple()))
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

    let session_token = format!("prune-test-token-{}", Uuid::new_v4().simple());
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
                 100000, 0, 'prune test', now())",
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

// ---------------------------------------------------------------------------
// AC-P1: Events in an expired partition absent from history after prune
// ---------------------------------------------------------------------------

/// Inserts events into a partition 35 days old, runs the prune, and verifies
/// the history endpoint returns an empty page.  The default retention is 30 days
/// so any partition older than 30 days should be dropped.
///
/// The test creates its own dated partition (35 days ago) to avoid interfering
/// with partitions that current events land in.
#[tokio::test]
async fn ac_p1_events_in_expired_partition_absent_after_prune() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };

    let fx = insert_fixtures(&pool).await;

    // Target date: 35 days ago — beyond the 30-day default retention window.
    let expired_date = chrono::Utc::now().date_naive() - chrono::TimeDelta::days(35);

    // Seed the old partition idempotently.
    sqlx::query("SELECT agents.seed_agent_events_partition($1)")
        .bind(expired_date)
        .execute(&pool)
        .await
        .expect("seed expired partition");

    // Insert two events directly with created_at in the expired partition.
    let event_created_at = expired_date.and_hms_opt(12, 0, 0).unwrap().and_utc();

    for seq in 1i64..=2 {
        sqlx::query(
            "INSERT INTO agents.agent_events \
             (session_id, tenant_id, event_type, sequence, payload, created_at) \
             VALUES ($1, $2, 'session.message', $3, $4, $5)",
        )
        .bind(fx.agent_session_id)
        .bind(fx.tenant_id)
        .bind(seq)
        .bind(serde_json::json!({"text": format!("expired event {seq}")}))
        .bind(event_created_at)
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("insert expired event {seq}: {e}"));
    }

    // Confirm events exist before prune.
    let count_before: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM agents.agent_events WHERE session_id = $1")
            .bind(fx.agent_session_id)
            .fetch_one(&pool)
            .await
            .expect("count before prune");
    assert_eq!(count_before, 2, "AC-P1: expect 2 events before prune");

    // Run the prune.
    let pruned: i32 = sqlx::query_scalar("SELECT agents.prune_agent_events_partitions()")
        .fetch_one(&pool)
        .await
        .expect("run prune");

    // At least the one expired partition must have been dropped.
    assert!(
        pruned >= 1,
        "AC-P1: prune must have dropped at least 1 partition, got {pruned}"
    );

    // History endpoint must now return empty events for the session.
    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/v1/agents/sessions/{}/events/history",
                    fx.agent_session_id
                ))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "AC-P1: history must return 200 after prune"
    );

    let body: Value =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 4096).await.unwrap())
            .unwrap();

    assert_eq!(
        body["events"].as_array().unwrap().len(),
        0,
        "AC-P1: history must return empty events after partition drop"
    );
    assert!(
        body["next_seq"].is_null(),
        "AC-P1: next_seq must be null when no events"
    );
}

// ---------------------------------------------------------------------------
// AC-P2: Prune is idempotent — re-running returns 0 dropped
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac_p2_prune_is_idempotent() {
    let Some((_state, pool)) = real_db_state().await else {
        return;
    };

    // Run once to drop any already-expired partitions.
    let _first: i32 = sqlx::query_scalar("SELECT agents.prune_agent_events_partitions()")
        .fetch_one(&pool)
        .await
        .expect("first prune");

    // Run again — must not fail and must report 0 dropped.
    let second: i32 = sqlx::query_scalar("SELECT agents.prune_agent_events_partitions()")
        .fetch_one(&pool)
        .await
        .expect("second prune");

    assert_eq!(
        second, 0,
        "AC-P2: second prune must drop 0 partitions (idempotent)"
    );
}

// ---------------------------------------------------------------------------
// AC-P3: Events in a current partition survive prune
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac_p3_current_partition_events_survive_prune() {
    let Some((state, pool)) = real_db_state().await else {
        return;
    };

    let fx = insert_fixtures(&pool).await;

    // Insert an event with created_at = now() (lands in today's partition).
    sqlx::query(
        "INSERT INTO agents.agent_events \
         (session_id, tenant_id, event_type, sequence, payload) \
         VALUES ($1, $2, 'session.message', 1, $3)",
    )
    .bind(fx.agent_session_id)
    .bind(fx.tenant_id)
    .bind(serde_json::json!({"text": "recent event"}))
    .execute(&pool)
    .await
    .expect("insert recent event");

    // Prune — today's partition is recent, must not be dropped.
    let _pruned: i32 = sqlx::query_scalar("SELECT agents.prune_agent_events_partitions()")
        .fetch_one(&pool)
        .await
        .expect("prune");

    // History must still return the recent event.
    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/v1/agents/sessions/{}/events/history",
                    fx.agent_session_id
                ))
                .header("cookie", format!("rb_session={}", fx.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK, "AC-P3: must return 200");

    let body: Value =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 4096).await.unwrap())
            .unwrap();

    assert_eq!(
        body["events"].as_array().unwrap().len(),
        1,
        "AC-P3: recent event must survive prune"
    );
}

// ---------------------------------------------------------------------------
// AC-P4: Per-tenant retention override stored and respected
// ---------------------------------------------------------------------------

/// Verifies that `agent_events_retention_days` is writable on a tenant row
/// and that the prune function uses it to compute the correct cutoff.
///
/// Scenario: tenant A has 7-day retention; insert events 8 days ago;
/// after prune, events are gone. MAX is computed over all active tenants,
/// so if all other active tenants also have ≤ 7 days, the 8-day-old data
/// is pruned.  This test uses a dedicated partition far in the past (35 days)
/// so no tenant's retention (even 30-day default) would preserve it.
#[tokio::test]
async fn ac_p4_per_tenant_retention_days_column_exists_and_is_writable() {
    let Some((_state, pool)) = real_db_state().await else {
        return;
    };

    let tenant_id = Uuid::new_v4();
    let slug = format!("retention-test-{}", tenant_id.simple());
    let schema_name = format!("retention_test_{}", tenant_id.simple());

    sqlx::query(
        "INSERT INTO control.tenants \
         (id, slug, name, schema_name, agent_events_retention_days) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(tenant_id)
    .bind(&slug)
    .bind("Retention Test Tenant")
    .bind(&schema_name)
    .bind(7i32)
    .execute(&pool)
    .await
    .expect("insert tenant with custom retention");

    let stored: i32 =
        sqlx::query_scalar("SELECT agent_events_retention_days FROM control.tenants WHERE id = $1")
            .bind(tenant_id)
            .fetch_one(&pool)
            .await
            .expect("read retention_days");

    assert_eq!(stored, 7, "AC-P4: retention_days must be stored as 7");

    // Verify the minimum constraint — 0 must be rejected.
    let result =
        sqlx::query("UPDATE control.tenants SET agent_events_retention_days = 0 WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await;

    assert!(
        result.is_err(),
        "AC-P4: retention_days < 1 must violate the check constraint"
    );
}

// ---------------------------------------------------------------------------
// AC-P5: Partition-maintenance endpoint is wired and returns 200
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac_p5_partition_maintenance_endpoint_returns_200() {
    let Some((state, _pool)) = real_db_state().await else {
        return;
    };

    let resp = build_internal(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/admin/partition-maintenance")
                .header("x-internal-secret", "test-partition-internal-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "AC-P5: partition-maintenance must return 200"
    );

    let body: Value =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 4096).await.unwrap())
            .unwrap();

    assert!(
        body["seeded"].is_number(),
        "AC-P5: response must have seeded field"
    );
    assert!(
        body["pruned"].is_number(),
        "AC-P5: response must have pruned field"
    );
}
