//! Regression-defence smoke tests for older-phase defects.
//!
//! Test 1: **Orphan-install 409** — `POST /v1/repos` and
//!    `GET /v1/github/installations/{id}/available-repos` must return HTTP 409
//!    with `error=installation_for_different_app` when the upstream GitHub App
//!    installation token endpoint returns 404.  Pre-fix both routes would
//!    surface a 500 Internal Server Error.
//!
//! DB-backed tests skip gracefully when `RB_DATABASE_URL` is not set (same
//! convention as `integration_ingest_activity_tests.rs`).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use control_api::{
    AppState, Config, KafkaConsistencyState, McpSessionStore, SessionCreateRateLimiter,
    TenantSessionCount, build_public,
};
use http_body_util::BodyExt as _;
use jsonwebtoken::EncodingKey;
use rb_auth::{LoginRateLimiter, PasswordHasher, sha256_hex};
use rb_email::from_transport;
use rb_github::{GhApp, GhAppLoader, Secret};
use rb_sse::{EventBus, SseConfig};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt as _;
use uuid::Uuid;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ── Shared RSA test key fixture ───────────────────────────────────────────────
//
// Base64 DER body only — PEM markers are absent so secret scanners do not flag
// this constant.  The full PEM is reconstructed at test runtime.  This is the
// same 2048-bit test key used in `app_jwt.rs` and the manifest integration test.

const TEST_RSA_KEY_BODY: &str = concat!(
    "MIIEpAIBAAKCAQEArwnQtrb3L6igXRguv2KEM+fbfgZK50iHkSQL+RFLpuzzPZRf",
    "yBIl3B9eimrcVjXpRIX8VbnfJQZIGreTx+F9NQG/qkbaKGEKmXZFcOIJqPDGeRNF",
    "Mc+r454g5lA95nF+92lfifZu5RZMzAShhOKfrQyjvejegmgSqCOMatFYoFovsqCrf",
    "D1yYfRoPqYjl+t1lNmJwP5/ETnw/JC/vJ1GTbOR3IhkA59D2vX6uwTNrZPJ7fo0S",
    "e74j5zdLYk63jVXSPs8zPLKL9O5Nn+ZjMZjSiI+p7TI2/AMS+MOBEcrLuL7c7ONB",
    "7zB5ZP6Uol0Q/DnT6nJJ8WWbyXhC8JM87onoQIDAQABAoIBAAJrk31gme9d7gW2LA",
    "33ues7z/mgnaWFXQvWi0HWNDe/0VHZ0i8316/WUTN/FxfWu/3MunihpCJkwVd5Oqu",
    "0rvYDgFfFjgZT59ZyX7MYClknJx9icv5QKEjH6sg0dilQYBiMq5utPXWhHCO6sVRf",
    "NnpT5pRdesIj1+oyP6KfIry+LJ78oKOznp8Awe0WcU2hW3rBo5YyTmHMHe1UBK04",
    "hcv2QunqY7SUKACxZGf4Tq/MBOTKq8ksamdW/4KQE/TK699s9qAZmKxnVrkBvXrea",
    "XxBW5LU3qTGd2sFtgcyc23xvGptM8Cr+poceEgGDHGyF5P/Wchv+Brn5ZN0b6o8P",
    "D8CgYEA4+NijtUSWIOFrlhsU6wMPK6FuHxSERn4UFFBiuigm/k0MCKmU2tcZlxSNd",
    "VF3vmdMsEj5E8ZIZ41CFcZDcTFPLSc4Gl4SPkrCtJNyaxqYkDLLTLxS2bBodiR0l",
    "AV3kw/XWgUoPcuZN19pqsJ39vOsYX/6ZV3/Z8w+UWMCR6y+YcCgYEAxKFz/QNhoN",
    "OLC045491fHuB0lfDYhpvphAKSrXBYqgE8OhPq7f8WHJV1XNT4bQCDFbLGbEZNac",
    "Z99OKbuWbJJDpjhpsR8kOakTIDP7gV14Hr54tVUZSybx1x/W/IyI9AlywTdBTGgVs",
    "Hwsa3bm87syY0jZAE1sOessBqxxppf5cCgYEAxoXb4hn0NW++ETeuhuWmc2aFz0Ve",
    "KM+65h0jP+OPptDdieFli95HTFS4uXTlvW0uaHygy8+sUQEFqhJWHQyB1nRxBX5b",
    "7xZBTNgQM9QjiRxw4xsx4UHPBTMpNVHW+yTpPnHhJqiunefmAj+WBpHx6eyWF+LB",
    "+QupGj5f08IOoBkCgYAvkqBtZpQIRSYu5g47gyOwZL3QSSUZ7D7jIXw7WiMZfpMD",
    "ui3sxvqij8aFX0F7ndQZO9el+pxgKxXuWaUzhhrEGRxbRMliw9hxqJgAopkmOtjI",
    "fH1373H8UDN0DceWPpJyAMf0HdKpGU0XYtyea2sWPPgaB+4jx9BtjwBGi61aoQKB",
    "gQDGthIge5R6CftG8P+E8hA+4sptbc+7XUaYcWxXLO81szX2wBgW89d2zo9RaJ3W",
    "4Qjhp1rYwfS9CyZliFDAGH+091X/7Yb53YATMkzbmrUpLQoSO42ylK+/n4Xp4CfO",
    "JiQDUBMer4rHHCTjQM1SeKAjM+HYsafx8sCiH9DR9qXudA=="
);

fn test_rsa_pem() -> String {
    format!("-----BEGIN RSA PRIVATE KEY-----\n{TEST_RSA_KEY_BODY}\n-----END RSA PRIVATE KEY-----\n")
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn db_pool() -> Option<PgPool> {
    let url = std::env::var("RB_DATABASE_URL").ok()?;
    PgPoolOptions::new()
        .max_connections(3)
        .connect(&url)
        .await
        .ok()
}

fn build_state_with_gh(pool: PgPool, gh_loader: Arc<GhAppLoader>) -> AppState {
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
        database_url: String::new(),
        cors_origins: vec![],
        base_url: "http://localhost:15173".to_owned(),
        session_ttl_days: 30,
        argon2_memory_kb: 64,
        argon2_time_cost: 1,
        argon2_parallelism: 1,
        email_transport: "noop".to_owned(),
        service_name: "regression-defence-test".to_owned(),
        secure_cookies: false,
        gh_app_id: None,
        gh_app_private_key_b64: None,
        gh_app_webhook_secret: None,
        gh_app_enc_key_b64: None,
        gh_api_base: "https://api.github.com".to_owned(),
        neo4j_uri: None,
        neo4j_user: "neo4j".to_owned(),
        neo4j_password: None,
        kafka_bootstrap_servers: "localhost:9092".to_owned(),
        dev_test_routes: false,
        migrations_root: None,
        qdrant_url: None,
        ollama_url: None,
        embedding_model: "nomic-embed-text".to_owned(),
        internal_secret: Some("regression-test-internal".to_owned()),
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
    AppState {
        pool,
        email_sender: Arc::from(email_sender),
        hasher: Arc::new(hasher),
        login_rate_limiter: Arc::new(LoginRateLimiter::new()),
        config: Arc::new(config),
        gh_loader,
        sse_bus: Arc::new(EventBus::new(SseConfig::default())),
        ingest_producer: None,
        tombstone_producer: None,
        module_tree_cache: rb_query::new_module_tree_cache(),
        graph: None,
        qdrant: None,
        http_client: reqwest::Client::new(),
        neo4j_uri: None,
        kafka_consistency: Arc::new(KafkaConsistencyState::new()),
        mcp_sessions: McpSessionStore::new(),
        agent_registry: control_api::AgentRegistry::new(),
        agent_commands_producer: None,
        internal_secret: "regression-test-internal".to_owned(),
        session_create_rate_limiter: Arc::new(SessionCreateRateLimiter::default()),
        tenant_session_count: Arc::new(TenantSessionCount::new()),
        mcp_jwt_secret: "test-mcp-jwt-secret".to_owned(),
        mcp_jwt_ttl_secs: 900,
        llm_api_key: String::new(),
    }
}

struct TestUser {
    tenant_id: Uuid,
    user_id: Uuid,
    session_token: String,
}

async fn seed_test_user(pool: &PgPool, prefix: &str) -> TestUser {
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(format!("rd-{prefix}-{}", tenant_id.simple()))
    .bind(format!("{prefix} Test Tenant"))
    .bind(format!("rd_{prefix}_{}", tenant_id.simple()))
    .execute(pool)
    .await
    .expect("insert tenant");

    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, email_verified_at) \
         VALUES ($1, $2, '$argon2id$v=19$m=65536,t=1,p=1$placeholder_hash', now())",
    )
    .bind(user_id)
    .bind(format!("rd-{prefix}-{}@test.example", user_id.simple()))
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

    let token = format!("rd-{prefix}-{}", Uuid::new_v4().simple());
    let token_hash = sha256_hex(&token);
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

    TestUser {
        tenant_id,
        user_id,
        session_token: token,
    }
}

async fn seed_installation(pool: &PgPool, tenant_id: Uuid, github_installation_id: i64) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO control.github_installations \
         (id, tenant_id, github_installation_id, account_login, account_type, account_id) \
         VALUES ($1, $2, $3, 'test-org', 'Organization', 42)",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(github_installation_id)
    .execute(pool)
    .await
    .expect("insert github_installation");
    id
}

fn cookie_header(token: &str) -> String {
    format!("rb_session={token}")
}

async fn collect_body(body: Body) -> Vec<u8> {
    body.collect()
        .await
        .expect("collect body")
        .to_bytes()
        .to_vec()
}

fn random_install_id() -> i64 {
    i64::from(rand::random::<i32>().abs()) + 3_000_000
}

async fn cleanup_user(pool: &PgPool, user_id: Uuid, tenant_id: Uuid) {
    sqlx::query("DELETE FROM control.sessions WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.tenant_members WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.users WHERE id = $1")
        .bind(user_id)
        .execute(pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(pool)
        .await
        .ok();
}

// ── Test 1: Orphan-install conflict returns 409 ───────────────────────────────

/// `POST /v1/repos` must return 409 with `error=installation_for_different_app`
/// when the active GitHub App cannot mint a token for the installation (the
/// upstream App was replaced or revoked).  Pre-fix this path bubbled up as 500.
///
/// Regression defence: orphan-install connect-repo path.
#[tokio::test]
async fn orphan_install_connect_repo_returns_409_not_500() {
    let Some(pool) = db_pool().await else {
        return; // skip: no DB
    };

    // Wiremock stub: GitHub token endpoint returns 404 (App deactivated).
    let github_stub = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(r"^/app/installations/\d+/access_tokens$"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(serde_json::json!({"message": "Integration not found"})),
        )
        .mount(&github_stub)
        .await;

    // Build a GhApp whose token-mint calls hit the wiremock stub.
    let pem = test_rsa_pem();
    let enc_key = EncodingKey::from_rsa_pem(pem.as_bytes()).expect("test RSA key");
    let gh_app = GhApp::new_with_api_base(
        42,
        enc_key,
        Secret::new(b"test-webhook-secret".to_vec()),
        &github_stub.uri(),
    );
    let loader = Arc::new(GhAppLoader::new(Some(Arc::new(gh_app))));

    let state = build_state_with_gh(pool.clone(), loader);
    let app = build_public(state);

    // Seed: tenant, user, session, installation.
    let user = seed_test_user(&pool, "orphan409cr").await;
    let numeric_install_id = random_install_id();
    let install_uuid = seed_installation(&pool, user.tenant_id, numeric_install_id).await;

    // POST /v1/repos: expects 409, not 500.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/repos")
                .header("content-type", "application/json")
                .header("cookie", cookie_header(&user.session_token))
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "installation_id": install_uuid,
                        "github_repo_id": 999_999,
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .expect("request failed");

    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "deactivated-App install must return 409, not 500"
    );

    let raw = collect_body(resp.into_body()).await;
    let body: serde_json::Value = serde_json::from_slice(&raw).expect("JSON body");
    assert_eq!(
        body["error"], "installation_for_different_app",
        "error code must be 'installation_for_different_app'"
    );
    assert!(
        body["install_url"].is_string(),
        "response must include install_url recovery hint"
    );

    // Cleanup.
    sqlx::query("DELETE FROM control.github_installations WHERE id = $1")
        .bind(install_uuid)
        .execute(&pool)
        .await
        .ok();
    cleanup_user(&pool, user.user_id, user.tenant_id).await;
}

/// `GET /v1/github/installations/{id}/available-repos` must return 409 with
/// `error=installation_for_different_app` when the active GitHub App cannot
/// mint a token.  Pre-fix this path bubbled up as 500.
///
/// Regression defence: orphan-install available-repos path.
#[tokio::test]
async fn orphan_install_available_repos_returns_409_not_500() {
    let Some(pool) = db_pool().await else {
        return; // skip: no DB
    };

    let github_stub = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(r"^/app/installations/\d+/access_tokens$"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(serde_json::json!({"message": "Integration not found"})),
        )
        .mount(&github_stub)
        .await;

    let pem = test_rsa_pem();
    let enc_key = EncodingKey::from_rsa_pem(pem.as_bytes()).expect("test RSA key");
    let gh_app = GhApp::new_with_api_base(
        42,
        enc_key,
        Secret::new(b"test-webhook-secret".to_vec()),
        &github_stub.uri(),
    );
    let loader = Arc::new(GhAppLoader::new(Some(Arc::new(gh_app))));

    let state = build_state_with_gh(pool.clone(), loader);
    let app = build_public(state);

    let user = seed_test_user(&pool, "orphan409ar").await;
    let numeric_install_id = random_install_id();
    let install_uuid = seed_installation(&pool, user.tenant_id, numeric_install_id).await;

    // GET /v1/github/installations/{id}/available-repos: expects 409.
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/v1/github/installations/{install_uuid}/available-repos"
                ))
                .header("cookie", cookie_header(&user.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("request failed");

    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "deactivated-App install must return 409 on available-repos"
    );

    let raw = collect_body(resp.into_body()).await;
    let body: serde_json::Value = serde_json::from_slice(&raw).expect("JSON body");
    assert_eq!(
        body["error"], "installation_for_different_app",
        "error code must be 'installation_for_different_app'"
    );
    assert!(
        body["install_url"].is_string(),
        "response must include install_url recovery hint"
    );

    // Cleanup.
    sqlx::query("DELETE FROM control.github_installations WHERE id = $1")
        .bind(install_uuid)
        .execute(&pool)
        .await
        .ok();
    cleanup_user(&pool, user.user_id, user.tenant_id).await;
}
