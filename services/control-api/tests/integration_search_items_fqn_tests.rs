//! Integration tests for RUSAA-2177: `search_items` must return `fqn`/`crate_name`
//! in `CitationV1` so the LLM can chain `search_items → get_item`.
//!
//! These tests skip without `RB_DATABASE_URL` (same pattern as hybrid isolation tests).
//! The sparse FTS leg drives results (Qdrant stub returns empty); `get_item` proves
//! the returned `fqn` is usable end-to-end.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use control_api::{AppState, Config, build_public};
use http_body_util::BodyExt as _;
use rb_auth::{LoginRateLimiter, McpTokenClaims, PasswordHasher, mint_mcp_token};
use rb_email::from_transport;
use rb_schemas::TenantId;
use rb_sse::{EventBus, SseConfig};
use rb_storage_qdrant::TenantVectorStore;
use rb_tenant::TenantCtx;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt as _;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const MCP_JWT_SECRET: &[u8] = b"test-fqn-chain-mcp-secret-long-enough-32b"; // gitleaks:allow

async fn connect_pool() -> Option<sqlx::PgPool> {
    let url = std::env::var("RB_DATABASE_URL").ok()?;
    PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .ok()
}

fn schema_name(tenant_id: Uuid) -> String {
    TenantCtx::new(TenantId::from(tenant_id))
        .schema_name()
        .to_owned()
}

async fn provision_tenant_schema(pool: &sqlx::PgPool, tenant_id: Uuid) {
    let schema = schema_name(tenant_id);
    sqlx::query(&format!("CREATE SCHEMA IF NOT EXISTS {schema}"))
        .execute(pool)
        .await
        .expect("create schema");

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
    .expect("create code_symbols");

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

async fn drop_tenant_schema(pool: &sqlx::PgPool, tenant_id: Uuid) {
    let schema = schema_name(tenant_id);
    sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .execute(pool)
        .await
        .ok();
}

async fn seed_tenant(pool: &sqlx::PgPool, tenant_id: Uuid) {
    let schema = schema_name(tenant_id);
    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name, status)
         VALUES ($1, $2, $3, $4, 'active')
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(tenant_id)
    .bind(format!(
        "fqn-chain-{}",
        &tenant_id.simple().to_string()[..8]
    ))
    .bind("FQN Chain Test")
    .bind(&schema)
    .execute(pool)
    .await
    .expect("seed tenant");
}

async fn seed_user(pool: &sqlx::PgPool, tenant_id: Uuid) -> Uuid {
    let user_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, email_verified_at)
         VALUES ($1, $2, '$argon2id$v=19$m=65536,t=1,p=1$placeholder', now())
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(user_id)
    .bind(format!("fqn-chain-{}@test.internal", user_id.simple()))
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

    user_id
}

/// Seed `github_installations` + `control.repos` so `dispatch_get_item` can verify ownership.
async fn seed_repo(pool: &sqlx::PgPool, tenant_id: Uuid, user_id: Uuid) -> Uuid {
    let repo_id = Uuid::new_v4();
    let install_id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO control.github_installations
             (id, tenant_id, github_installation_id, account_login, account_type, account_id)
         VALUES ($1, $2, $3, 'fqn-chain-test', 'User', $4)
         ON CONFLICT DO NOTHING",
    )
    .bind(install_id)
    .bind(tenant_id)
    .bind(i64::from(rand_i32()))
    .bind(i64::from(rand_i32()))
    .execute(pool)
    .await
    .expect("seed installation");

    sqlx::query(
        "INSERT INTO control.repos
             (id, tenant_id, installation_id, github_repo_id, full_name, default_branch,
              status, connected_by)
         VALUES ($1, $2, $3, $4, 'org/fqn-chain', 'main', 'ready', $5)
         ON CONFLICT DO NOTHING",
    )
    .bind(repo_id)
    .bind(tenant_id)
    .bind(install_id)
    .bind(i64::from(rand_i32()))
    .bind(user_id)
    .execute(pool)
    .await
    .expect("seed repo");

    repo_id
}

fn rand_i32() -> i32 {
    i32::from_ne_bytes(Uuid::new_v4().as_bytes()[..4].try_into().unwrap()).abs()
}

async fn insert_symbol(pool: &sqlx::PgPool, tenant_id: Uuid, repo_id: Uuid, fqn: &str) {
    let schema = schema_name(tenant_id);
    sqlx::query(&format!(
        "INSERT INTO {schema}.code_symbols
             (repo_id, fqn, kind, source_path, line_start, line_end, source_text)
         VALUES ($1, $2, 'function', 'src/lib.rs', 1, 10, $2)
         ON CONFLICT DO NOTHING"
    ))
    .bind(repo_id)
    .bind(fqn)
    .execute(pool)
    .await
    .expect("insert symbol");
}

fn build_state(pool: sqlx::PgPool, qdrant_url: &str, ollama_url: &str) -> AppState {
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
        service_name: "control-api-fqn-chain-test".to_owned(),
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
        internal_secret: Some("test-fqn-chain-internal-secret".to_owned()),
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
        rerank_candidate_cap: 50,
        llm_token_ceiling_per_tenant: 0,
        hybrid_search_enabled: true,
        multi_query_n: 1,
        rewrite_model: String::new(),
        multi_query_token_budget: 0,
        rerank_enabled: false,
        rerank_model_dir: std::path::PathBuf::from("/models/rerank"),
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
        internal_secret: "test-fqn-chain-internal-secret".to_owned(),
        session_create_rate_limiter: Arc::new(control_api::SessionCreateRateLimiter::default()),
        tenant_session_count: Arc::new(control_api::TenantSessionCount::new()),
        mcp_jwt_secret: std::str::from_utf8(MCP_JWT_SECRET).unwrap().to_owned(),
        mcp_jwt_ttl_secs: 900,
        llm_api_key: String::new(),
        reranker: None,
        llm_tenant_tokens: std::sync::Arc::new(control_api::TenantLlmTokenCounter::new()),
    }
}

const FAKE_VECTOR: [f32; 3] = [0.1, 0.2, 0.3];

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

/// `AC4 integration` — `search_items → get_item` chain:
/// 1. `search_items` returns a citation with `fqn` populated (sparse FTS leg)
/// 2. `fqn` + `repo_id` extracted from citation and fed to `get_item`
/// 3. `get_item` returns the matching symbol
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn search_items_fqn_enables_get_item_chain() {
    let Some(pool) = connect_pool().await else {
        return;
    };

    let tenant_id = Uuid::new_v4();
    let target_fqn = format!(
        "fqn_chain_test_{}::TargetStruct",
        &tenant_id.simple().to_string()[..8]
    );

    provision_tenant_schema(&pool, tenant_id).await;
    seed_tenant(&pool, tenant_id).await;
    let user_id = seed_user(&pool, tenant_id).await;
    let repo_id = seed_repo(&pool, tenant_id, user_id).await;
    insert_symbol(&pool, tenant_id, repo_id, &target_fqn).await;

    let ollama_stub = MockServer::start().await;
    let qdrant_stub = MockServer::start().await;
    mount_stubs(&ollama_stub, &qdrant_stub).await;

    let state = build_state(pool.clone(), &qdrant_stub.uri(), &ollama_stub.uri());
    let app = build_public(state);

    let jwt = mint_mcp_token(
        MCP_JWT_SECRET,
        900,
        McpTokenClaims {
            sub: Uuid::new_v4(),
            tenant_id,
            user_id,
        },
    )
    .expect("mint MCP JWT");

    // --- Step 1: initialize MCP session ---
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
                            "clientInfo": { "name": "fqn-chain-test", "version": "0.1" }
                        }
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(init_resp.status(), StatusCode::OK);
    let mcp_session_id = init_resp
        .headers()
        .get("Mcp-Session-Id")
        .expect("initialize must return Mcp-Session-Id")
        .to_str()
        .unwrap()
        .to_owned();

    // --- Step 2: call search_items ---
    let search_resp = app
        .clone()
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
                                "query": target_fqn.replace("::", " "),
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
        search_resp.status(),
        StatusCode::OK,
        "search_items must return 200"
    );

    let search_raw = search_resp.into_body().collect().await.unwrap().to_bytes();
    let search_body: serde_json::Value = serde_json::from_slice(&search_raw).unwrap();

    let tool_text = search_body
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("[]");
    let citations: serde_json::Value =
        serde_json::from_str(tool_text).unwrap_or(serde_json::json!([]));

    let citations_arr = citations.as_array().expect("citations must be an array");
    assert!(
        !citations_arr.is_empty(),
        "search_items must return at least one citation for the seeded symbol"
    );

    // Verify fqn is present in the first citation.
    let first = &citations_arr[0];
    let returned_fqn = first["fqn"]
        .as_str()
        .expect("AC4: citation must contain 'fqn' field");
    let returned_crate = first["crate_name"]
        .as_str()
        .expect("AC4: citation must contain 'crate_name' field");
    let returned_repo_id = first["repo_id"]
        .as_str()
        .expect("AC4: citation must contain 'repo_id'");

    assert_eq!(returned_fqn, target_fqn, "fqn must match the seeded symbol");
    assert_eq!(
        returned_crate,
        target_fqn.split("::").next().unwrap_or(&target_fqn),
        "crate_name must be the leading :: segment"
    );

    // --- Step 3: feed fqn + repo_id back to get_item ---
    let get_resp = app
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
                        "id": 3,
                        "method": "tools/call",
                        "params": {
                            "name": "get_item",
                            "arguments": {
                                "repo_id": returned_repo_id,
                                "fqn": returned_fqn
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
        get_resp.status(),
        StatusCode::OK,
        "get_item must return 200"
    );

    let get_raw = get_resp.into_body().collect().await.unwrap().to_bytes();
    let get_body: serde_json::Value = serde_json::from_slice(&get_raw).unwrap();

    let get_text = get_body
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .expect("get_item must return text content");

    // get_item returns the symbol JSON or "No symbol found…"
    assert!(
        !get_text.contains("No symbol found"),
        "AC4: get_item must find the symbol when given the fqn from search_items; got: {get_text}"
    );

    let symbol: serde_json::Value =
        serde_json::from_str(get_text).expect("get_item result text must be valid JSON");
    assert_eq!(
        symbol["fqn"].as_str().unwrap_or(""),
        target_fqn,
        "AC4: get_item must return matching symbol fqn"
    );

    // Cleanup
    drop_tenant_schema(&pool, tenant_id).await;
    sqlx::query("DELETE FROM control.repos WHERE id = $1")
        .bind(repo_id)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.github_installations WHERE tenant_id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}
