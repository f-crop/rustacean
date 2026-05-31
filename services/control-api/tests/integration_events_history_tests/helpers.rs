//! Shared fixtures and helpers for `integration_events_history_tests`.

use std::sync::Arc;

use rb_auth::{LoginRateLimiter, PasswordHasher, sha256_hex};
use rb_email::from_transport;
use rb_sse::{EventBus, SseConfig};
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use control_api::{
    AppState, Config, KafkaConsistencyState, SessionCreateRateLimiter, TenantSessionCount,
};

// ---------------------------------------------------------------------------
// State builder
// ---------------------------------------------------------------------------

pub async fn real_db_state() -> Option<(AppState, PgPool)> {
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
        admin_token: None,
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

pub struct Fixtures {
    pub session_token: String,
    pub agent_session_id: Uuid,
    pub tenant_id: Uuid,
    #[allow(dead_code)]
    pub user_id: Uuid,
}

pub async fn insert_fixtures(pool: &PgPool) -> Fixtures {
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
pub async fn insert_n_events(pool: &PgPool, session_id: Uuid, tenant_id: Uuid, n: usize) {
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

// ---------------------------------------------------------------------------
// URI builders
// ---------------------------------------------------------------------------

pub fn history_uri(session_id: Uuid) -> String {
    format!("/v1/agents/sessions/{session_id}/events/history")
}

pub fn history_uri_with_params(session_id: Uuid, after: Option<i64>, limit: Option<i64>) -> String {
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
