//! AC5 tenant isolation regression tests for hybrid retrieval (ADR-014 §10).
//! Seeds two tenants, queries one, asserts zero rows from the other on both
//! dense (Qdrant must-filter) and sparse (PG FTS schema routing) legs at both
//! call sites (`POST /v1/search` and MCP `search_items`). Skips without `RB_DATABASE_URL`.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use control_api::{AppState, Config, build_public};
use http_body_util::BodyExt as _;
use rb_auth::{LoginRateLimiter, McpTokenClaims, PasswordHasher, mint_mcp_token, sha256_hex};
use rb_email::from_transport;
use rb_query::{HybridSearchOptions, hybrid_search};
use rb_schemas::TenantId;
use rb_sse::{EventBus, SseConfig};
use rb_storage_qdrant::TenantVectorStore;
use rb_tenant::TenantCtx;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt as _;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const MCP_JWT_SECRET: &[u8] = b"test-hybrid-isolation-mcp-secret-long-enough"; // gitleaks:allow

async fn connect_pool() -> Option<sqlx::PgPool> {
    let url = std::env::var("RB_DATABASE_URL").ok()?;
    PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .ok()
}

fn schema_name_for(tenant_id: Uuid) -> String {
    TenantCtx::new(TenantId::from(tenant_id))
        .schema_name()
        .to_owned()
}

/// Create a per-tenant schema with `code_symbols` + `fts` GENERATED column (idempotent).
async fn provision_tenant_schema(pool: &sqlx::PgPool, tenant_id: Uuid) {
    let schema = schema_name_for(tenant_id);

    sqlx::query(&format!("CREATE SCHEMA IF NOT EXISTS {schema}"))
        .execute(pool)
        .await
        .expect("create tenant schema");

    sqlx::query(&format!(
        "CREATE TABLE IF NOT EXISTS {schema}.code_symbols (
            id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
            repo_id     UUID        NOT NULL,
            fqn         TEXT        NOT NULL,
            kind        TEXT        NOT NULL DEFAULT 'function',
            source_path TEXT,
            line_start  INTEGER,
            line_end    INTEGER,
            source_text TEXT,
            created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
            updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
        )"
    ))
    .execute(pool)
    .await
    .expect("create code_symbols table");

    // Apply migration 007: add fts GENERATED column if absent.
    sqlx::query(&format!(
        "ALTER TABLE {schema}.code_symbols
             ADD COLUMN IF NOT EXISTS fts tsvector
             GENERATED ALWAYS AS (
                 to_tsvector('simple',
                     coalesce(fqn, '') || ' ' || coalesce(source_text, ''))
             ) STORED"
    ))
    .execute(pool)
    .await
    .expect("add fts column");
}

/// Drop the tenant schema created by `provision_tenant_schema`.
async fn drop_tenant_schema(pool: &sqlx::PgPool, tenant_id: Uuid) {
    let schema = schema_name_for(tenant_id);
    sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .execute(pool)
        .await
        .ok();
}

/// Insert a single code symbol into a tenant's schema.
async fn insert_symbol(pool: &sqlx::PgPool, tenant_id: Uuid, repo_id: Uuid, fqn: &str) {
    let schema = schema_name_for(tenant_id);
    sqlx::query(&format!(
        "INSERT INTO {schema}.code_symbols (repo_id, fqn, kind, source_text)
         VALUES ($1, $2, 'function', $2)
         ON CONFLICT DO NOTHING"
    ))
    .bind(repo_id)
    .bind(fqn)
    .execute(pool)
    .await
    .expect("insert symbol");
}

/// Seed a row in `control.tenants` for the given UUID.
async fn seed_control_tenant(pool: &sqlx::PgPool, tenant_id: Uuid) {
    let schema = schema_name_for(tenant_id);
    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name, status)
         VALUES ($1, $2, $3, $4, 'active')
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(tenant_id)
    .bind(format!(
        "hybrid-iso-{}",
        &tenant_id.simple().to_string()[..8]
    ))
    .bind("Hybrid Isolation Test")
    .bind(&schema)
    .execute(pool)
    .await
    .expect("seed control tenant");
}

/// Seed a user + session for a tenant; returns the plaintext session token.
async fn seed_user_session(pool: &sqlx::PgPool, tenant_id: Uuid) -> String {
    let user_id = Uuid::new_v4();
    let session_token = format!("hybrid-iso-tok-{}", Uuid::new_v4().simple());
    let token_hash = sha256_hex(&session_token);

    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, email_verified_at)
         VALUES ($1, $2, '$argon2id$v=19$m=65536,t=1,p=1$placeholder', now())
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(user_id)
    .bind(format!("hybrid-iso-{}@test.internal", user_id.simple()))
    .execute(pool)
    .await
    .expect("seed user");

    sqlx::query(
        "INSERT INTO control.tenant_members (tenant_id, user_id, role)
         VALUES ($1, $2, 'owner') ON CONFLICT DO NOTHING",
    )
    .bind(tenant_id)
    .bind(user_id)
    .execute(pool)
    .await
    .expect("seed tenant_member");

    sqlx::query(
        "INSERT INTO control.sessions (id, user_id, tenant_id, token_hash, expires_at)
         VALUES ($1, $2, $3, $4, now() + interval '30 days')
         ON CONFLICT DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(tenant_id)
    .bind(&token_hash)
    .execute(pool)
    .await
    .expect("seed session");

    session_token
}

/// Build an `AppState` with `hybrid_search_enabled: true`, a wiremocked Qdrant
/// URL, and a wiremocked Ollama URL.
fn build_hybrid_state(pool: sqlx::PgPool, qdrant_url: &str, ollama_url: &str) -> AppState {
    let smtp = rb_email::SmtpConfig {
        host: String::new(),
        port: 587,
        username: String::new(),
        password: String::new(),
        from_address: "test@example.com".to_owned(),
    };
    let email_sender = from_transport("noop", &smtp).expect("noop transport");
    let hasher = PasswordHasher::from_config(64, 1, 1).expect("hasher");

    let config = Config {
        listen_addr: "127.0.0.1:0".to_owned(),
        database_url: "unused".to_owned(),
        cors_origins: vec![],
        base_url: "http://localhost:8080".to_owned(),
        session_ttl_days: 30,
        argon2_memory_kb: 64,
        argon2_time_cost: 1,
        argon2_parallelism: 1,
        email_transport: "noop".to_owned(),
        service_name: "control-api-hybrid-isolation-test".to_owned(),
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
        qdrant_url: Some(qdrant_url.to_owned()),
        ollama_url: Some(ollama_url.to_owned()),
        embedding_model: "nomic-embed-text".to_owned(),
        internal_secret: Some("test-hybrid-internal-secret".to_owned()),
        internal_listen_addr: "127.0.0.1:0".to_owned(),
        session_create_rate_limit: 100,
        session_create_window_secs: 60,
        tenant_session_cap: 100,
        admin_token: None,
        chat_panel_enabled: false,
        tempo_base_url: "http://localhost:3000".to_owned(),
        mcp_jwt_secret: Some(std::str::from_utf8(MCP_JWT_SECRET).unwrap().to_owned()),
        mcp_jwt_ttl_secs: 900,
        llm_api_key: None,
        hybrid_search_enabled: true,
    };

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
        qdrant: Some(Arc::new(TenantVectorStore::new(qdrant_url))),
        http_client: reqwest::Client::new(),
        neo4j_uri: None,
        kafka_consistency: Arc::new(control_api::KafkaConsistencyState::new()),
        mcp_sessions: control_api::McpSessionStore::new(),
        agent_registry: control_api::AgentRegistry::new(),
        agent_commands_producer: None,
        internal_secret: "test-hybrid-internal-secret".to_owned(),
        session_create_rate_limiter: Arc::new(control_api::SessionCreateRateLimiter::default()),
        tenant_session_count: Arc::new(control_api::TenantSessionCount::new()),
        mcp_jwt_secret: std::str::from_utf8(MCP_JWT_SECRET).unwrap().to_owned(),
        mcp_jwt_ttl_secs: 900,
        llm_api_key: String::new(),
    }
}

const FAKE_VECTOR: [f32; 3] = [0.1, 0.2, 0.3];

/// Mount Ollama embedding stub and Qdrant stub (dense leg returns empty; sparse leg drives results).
async fn mount_stubs(ollama: &MockServer, qdrant: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "embedding": FAKE_VECTOR
        })))
        .mount(ollama)
        .await;

    Mock::given(method("POST"))
        .and(path("/collections/rb_embeddings/points/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [],
            "status": "ok",
            "time": 0.001
        })))
        .mount(qdrant)
        .await;
}

/// Assert every Qdrant request carries `expected_tenant_id` in must-filter and not `cross_tenant_id`.
async fn assert_qdrant_must_filter(
    qdrant: &MockServer,
    expected_tenant_id: Uuid,
    cross_tenant_id: Uuid,
) {
    let reqs = qdrant.received_requests().await.unwrap_or_default();
    assert!(
        !reqs.is_empty(),
        "AC5 dense leg: Qdrant must receive at least one request"
    );

    for req in &reqs {
        let body: serde_json::Value =
            serde_json::from_slice(&req.body).expect("Qdrant request must be valid JSON");

        let must_conditions = body
            .pointer("/filter/must")
            .and_then(serde_json::Value::as_array)
            .expect("AC5 dense leg: Qdrant request must have /filter/must array");

        let tenant_cond = must_conditions
            .iter()
            .find(|c| c.get("key").and_then(|k| k.as_str()) == Some("tenant_id"))
            .expect("AC5 dense leg: /filter/must must contain a tenant_id condition");

        let filter_value = tenant_cond
            .pointer("/match/value")
            .and_then(serde_json::Value::as_str)
            .expect("AC5 dense leg: tenant_id condition must have /match/value");

        assert_eq!(
            filter_value,
            expected_tenant_id.to_string(),
            "AC5 dense leg: Qdrant must-filter must name the querying tenant, got {filter_value}"
        );
        assert_ne!(
            filter_value,
            cross_tenant_id.to_string(),
            "AC5 dense leg: Qdrant must-filter must NOT name the cross-tenant UUID"
        );
    }
}

/// AC5a — `POST /v1/search`: tainted symbol seeded in tenant_B returns zero results queried as tenant_A.
/// Both sparse (PG FTS schema routing) and dense (Qdrant must-filter) legs verified.
#[tokio::test]
async fn ac5_search_site_two_tenants_zero_cross_tenant_rows() {
    let Some(pool) = connect_pool().await else {
        return;
    };

    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let repo_b = Uuid::new_v4();

    provision_tenant_schema(&pool, tenant_a).await;
    provision_tenant_schema(&pool, tenant_b).await;
    insert_symbol(&pool, tenant_b, repo_b, "tainted_b_only::ToxicFn").await;

    // Register tenant_A in control schema so the auth middleware accepts the session.
    seed_control_tenant(&pool, tenant_a).await;
    let session_token = seed_user_session(&pool, tenant_a).await;

    let ollama_stub = MockServer::start().await;
    let qdrant_stub = MockServer::start().await;
    mount_stubs(&ollama_stub, &qdrant_stub).await;

    let state = build_hybrid_state(pool.clone(), &qdrant_stub.uri(), &ollama_stub.uri());

    let resp = build_public(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/search")
                .header("content-type", "application/json")
                .header("cookie", format!("rb_session={session_token}"))
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "q": "tainted_b_only ToxicFn"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "AC5 search site: /v1/search must return 200"
    );

    let raw = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&raw).unwrap();

    // Sparse leg: tenant_A.code_symbols is empty, so results must be empty.
    assert_eq!(
        body["results"].as_array().map(Vec::len).unwrap_or(0),
        0,
        "AC5 sparse leg (search site): must return zero results — \
         tainted symbol lives in tenant_B's schema only"
    );

    assert_qdrant_must_filter(&qdrant_stub, tenant_a, tenant_b).await;

    drop_tenant_schema(&pool, tenant_a).await;
    drop_tenant_schema(&pool, tenant_b).await;
    sqlx::query("DELETE FROM control.tenants WHERE id = $1")
        .bind(tenant_a)
        .execute(&pool)
        .await
        .ok();
}

/// AC5b — MCP `search_items` (dispatch.rs): same two-tenant isolation proof via MCP JWT + tools/call.
#[tokio::test]
async fn ac5_dispatch_site_two_tenants_zero_cross_tenant_rows() {
    let Some(pool) = connect_pool().await else {
        return;
    };

    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let repo_b = Uuid::new_v4();

    provision_tenant_schema(&pool, tenant_a).await;
    provision_tenant_schema(&pool, tenant_b).await;
    insert_symbol(&pool, tenant_b, repo_b, "tainted_b_only::ToxicFn").await;

    seed_control_tenant(&pool, tenant_a).await;

    let ollama_stub = MockServer::start().await;
    let qdrant_stub = MockServer::start().await;
    mount_stubs(&ollama_stub, &qdrant_stub).await;

    let state = build_hybrid_state(pool.clone(), &qdrant_stub.uri(), &ollama_stub.uri());
    let app = build_public(state);

    let jwt = mint_mcp_token(
        MCP_JWT_SECRET,
        900,
        McpTokenClaims {
            sub: Uuid::new_v4(),
            tenant_id: tenant_a,
            user_id: Uuid::new_v4(),
        },
    )
    .expect("mint MCP JWT");

    let init_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {jwt}"))
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "initialize",
                        "params": {
                            "protocolVersion": "2024-11-05",
                            "capabilities": {},
                            "clientInfo": { "name": "ac5-test", "version": "0.1" }
                        }
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        init_resp.status(),
        StatusCode::OK,
        "AC5 dispatch site: MCP initialize must return 200"
    );

    let mcp_session_id = init_resp
        .headers()
        .get("Mcp-Session-Id")
        .expect("AC5 dispatch site: initialize must return Mcp-Session-Id header")
        .to_str()
        .unwrap()
        .to_owned();

    let call_resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {jwt}"))
                .header("Mcp-Session-Id", &mcp_session_id)
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 2,
                        "method": "tools/call",
                        "params": {
                            "name": "search_items",
                            "arguments": {
                                "query": "tainted_b_only ToxicFn",
                                "limit": 10
                            }
                        }
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        call_resp.status(),
        StatusCode::OK,
        "AC5 dispatch site: MCP tools/call must return 200"
    );

    let raw = call_resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&raw).unwrap();

    // Extract citations from the tool result text field.
    let tool_text = body
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("[]");
    let citations: serde_json::Value =
        serde_json::from_str(tool_text).unwrap_or(serde_json::json!([]));

    assert_eq!(
        citations.as_array().map(Vec::len).unwrap_or(0),
        0,
        "AC5 sparse leg (dispatch site): MCP search_items must return zero citations — \
         tainted symbol lives in tenant_B's schema only"
    );

    assert_qdrant_must_filter(&qdrant_stub, tenant_a, tenant_b).await;

    drop_tenant_schema(&pool, tenant_a).await;
    drop_tenant_schema(&pool, tenant_b).await;
    sqlx::query("DELETE FROM control.tenants WHERE id = $1")
        .bind(tenant_a)
        .execute(&pool)
        .await
        .ok();
}

/// AC5c — direct crate API proof: `hybrid_search` itself is isolated (complements HTTP-layer tests above).
#[tokio::test]
async fn ac5_direct_hybrid_search_sparse_leg_isolation() {
    let Some(pool) = connect_pool().await else {
        return;
    };

    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let repo_b = Uuid::new_v4();

    provision_tenant_schema(&pool, tenant_a).await;
    provision_tenant_schema(&pool, tenant_b).await;
    insert_symbol(&pool, tenant_b, repo_b, "cross_tenant_b::ExclusiveFn").await;

    let qdrant_stub = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/collections/rb_embeddings/points/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [],
            "status": "ok",
            "time": 0.001
        })))
        .mount(&qdrant_stub)
        .await;

    let store = TenantVectorStore::new(&qdrant_stub.uri());
    let tenant_a_id = TenantId::from(tenant_a);

    let results: Vec<rb_query::HybridHit> = hybrid_search(
        &pool,
        &store,
        &tenant_a_id,
        &FAKE_VECTOR,
        "cross_tenant_b ExclusiveFn",
        HybridSearchOptions {
            limit: 10,
            repo_id: None,
        },
    )
    .await
    .expect("hybrid_search must not error");

    assert!(
        results.is_empty(),
        "AC5 sparse leg (direct): hybrid_search for tenant_A must return zero hits — \
         cross_tenant_b::ExclusiveFn only exists in tenant_B's schema"
    );

    assert_qdrant_must_filter(&qdrant_stub, tenant_a, tenant_b).await;

    drop_tenant_schema(&pool, tenant_a).await;
    drop_tenant_schema(&pool, tenant_b).await;
}
