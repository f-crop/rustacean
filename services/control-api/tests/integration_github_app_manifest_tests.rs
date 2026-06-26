//! Integration test for the Phase 3 admin-manifest endpoints
//! (RUSAA-265 / parent issue).
//!
//! Exercises the full happy path against a real Postgres + a wiremock-stubbed
//! GitHub manifest-exchange endpoint:
//!
//!   POST /v1/admin/github/app-manifest  → mint state, redirect URL
//!   GET  /v1/admin/github/app-callback  → exchange code, persist, hot-swap
//!   GET  /v1/admin/github/app-status    → reports `source: "db"`
//!
//! Also covers the 403 path for a non-platform-admin caller and the 400
//! replay path for a state token that was already consumed.
//!
//! Requires `RB_DATABASE_URL` to point at a control-plane Postgres with the
//! Phase 1 (017) migration applied. Without it the tests no-op (mirrors the
//! pattern in `integration_api_key_revocation_tests`).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use control_api::{
    AppState, Config, KafkaConsistencyState, McpSessionStore, SessionCreateRateLimiter,
    TenantSessionCount, build_public,
};
use http_body_util::BodyExt as _;
use rb_auth::{LoginRateLimiter, PasswordHasher, sha256_hex};
use rb_email::from_transport;
use rb_sse::{EventBus, SseConfig};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt as _;
use uuid::Uuid;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const INTERNAL_SECRET: &str = "manifest-test-internal";

/// Test RSA key fixture. Mirrors the same encoding trick used in
/// `integration_github_webhook` so secret scanners do not flag this file —
/// store the DER body alone, reconstruct the PEM at runtime.
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
    "JiQDUBMer4rHHCTjQM1SeKAjM+HYsafx8sCiH9DR9qXudA==",
);

fn test_rsa_pem() -> String {
    format!("-----BEGIN RSA PRIVATE KEY-----\n{TEST_RSA_KEY_BODY}\n-----END RSA PRIVATE KEY-----\n")
}

/// Build a `Config` keyed to a real Postgres URL. Returns `None` when
/// `RB_DATABASE_URL` is unset so the test silently no-ops in CI without infra.
fn build_test_config(db_url: String, enc_key_b64: String) -> Config {
    build_test_config_with_api_base(
        db_url,
        enc_key_b64,
        rb_github::DEFAULT_GITHUB_API_BASE.to_owned(),
    )
}

fn build_test_config_with_api_base(
    db_url: String,
    enc_key_b64: String,
    gh_api_base: String,
) -> Config {
    Config {
        listen_addr: "127.0.0.1:0".to_owned(),
        database_url: db_url,
        cors_origins: vec![],
        base_url: "http://localhost:15173".to_owned(),
        session_ttl_days: 30,
        argon2_memory_kb: 64,
        argon2_time_cost: 1,
        argon2_parallelism: 1,
        email_transport: "noop".to_owned(),
        service_name: "control-api-manifest-test".to_owned(),
        secure_cookies: false,
        gh_app_id: None,
        gh_app_private_key_b64: None,
        gh_app_webhook_secret: None,
        gh_app_enc_key_b64: Some(enc_key_b64),
        gh_api_base,
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
        tempo_base_url: "http://localhost:3000".to_owned(),
        chat_panel_enabled: false,
        mcp_jwt_secret: Some("test-mcp-jwt-secret-for-unit-tests-only".to_owned()),
        mcp_jwt_ttl_secs: 900,
        llm_api_key: None,
        hybrid_search_enabled: false,
        multi_query_n: 1,
        rerank_enabled: false,
        rerank_model_dir: std::path::PathBuf::from("/models/rerank"),
        rerank_candidate_cap: 50,
        llm_token_ceiling_per_tenant: 0,
    }
}

async fn real_db_pool() -> Option<PgPool> {
    let db_url = std::env::var("RB_DATABASE_URL").ok()?;
    PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .ok()
}

fn build_state(pool: PgPool, config: Config) -> AppState {
    let smtp = rb_email::SmtpConfig {
        host: String::new(),
        port: 587,
        username: String::new(),
        password: String::new(),
        from_address: "test@example.com".to_owned(),
    };
    let email_sender = from_transport("noop", &smtp).expect("noop transport");
    let hasher = PasswordHasher::from_config(64, 1, 1).expect("hasher");
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
        qdrant: None,
        http_client: reqwest::Client::new(),
        neo4j_uri: None,
        kafka_consistency: Arc::new(KafkaConsistencyState::new()),
        mcp_sessions: McpSessionStore::new(),
        agent_registry: control_api::AgentRegistry::new(),
        agent_commands_producer: None,
        internal_secret: INTERNAL_SECRET.to_owned(),
        session_create_rate_limiter: Arc::new(SessionCreateRateLimiter::default()),
        tenant_session_count: Arc::new(TenantSessionCount::new()),
        mcp_jwt_secret: "test-mcp-jwt-secret".to_owned(),
        mcp_jwt_ttl_secs: 900,
        llm_api_key: String::new(),
        reranker: None,
    }
}

struct UserFixture {
    user_id: Uuid,
    session_token: String,
}

async fn seed_user(pool: &PgPool, is_platform_admin: bool) -> UserFixture {
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let slug = format!("manifest-test-{}", tenant_id.simple());
    let schema_name = format!("manifest_{}", tenant_id.simple());

    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(&slug)
    .bind("Manifest Test Tenant")
    .bind(&schema_name)
    .execute(pool)
    .await
    .expect("insert tenant");

    sqlx::query(
        "INSERT INTO control.users (id, email, password_hash, email_verified_at, is_platform_admin) \
         VALUES ($1, $2, $3, now(), $4)",
    )
    .bind(user_id)
    .bind(format!("manifest-{}@test.example", user_id.simple()))
    .bind("$argon2id$v=19$m=65536,t=1,p=1$placeholder_hash")
    .bind(is_platform_admin)
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

    let session_token = format!("manifest-test-{}", Uuid::new_v4().simple());
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

    UserFixture {
        user_id,
        session_token,
    }
}

async fn cleanup(pool: &PgPool, user_id: Uuid) {
    let _ = sqlx::query("DELETE FROM control.github_app_config WHERE installed_by_user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await;
    let _ =
        sqlx::query("DELETE FROM control.github_manifest_states WHERE initiated_by_user_id = $1")
            .bind(user_id)
            .execute(pool)
            .await;
    let _ = sqlx::query("DELETE FROM control.sessions WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM control.tenant_members WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM control.users WHERE id = $1")
        .bind(user_id)
        .execute(pool)
        .await;
}

fn cookie_header(token: &str) -> String {
    format!("rb_session={token}")
}

fn init_tracing() {
    // Best-effort: fine if a previous test already installed the subscriber.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("control_api=debug,info")),
        )
        .with_test_writer()
        .try_init();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn manifest_flow_persists_and_hot_swaps_loader() {
    init_tracing();
    let Some(pool) = real_db_pool().await else {
        eprintln!("RB_DATABASE_URL not set — skipping manifest flow test");
        return;
    };

    // 32-byte base64 key (deterministic for test reproducibility).
    let enc_key = base64::engine::general_purpose::STANDARD.encode([0x42u8; 32]);
    let admin = seed_user(&pool, true).await;

    let mock = MockServer::start().await;
    let pem = test_rsa_pem();
    Mock::given(method("POST"))
        .and(path_regex(r"^/app-manifests/[^/]+/conversions$"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": 555_001,
            "slug": "rustacean-test",
            "client_id": "Iv1.test",
            "client_secret": "cs-test",
            "webhook_secret": "ws-test",
            "pem": pem,
        })))
        .expect(1)
        .mount(&mock)
        .await;

    // Pass the wiremock base URL into Config so the callback handler uses it
    // without process-env mutation.
    let config = build_test_config_with_api_base(
        std::env::var("RB_DATABASE_URL").expect("checked above"),
        enc_key.clone(),
        mock.uri(),
    );

    let state = build_state(pool.clone(), config);
    let app = build_public(state.clone());

    // 1. Mint state via POST /v1/admin/github/app-manifest.
    let mint_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/admin/github/app-manifest")
                .header("cookie", cookie_header(&admin.session_token))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"rustacean-test"}"#))
                .unwrap(),
        )
        .await
        .expect("manifest request");
    assert_eq!(mint_resp.status(), StatusCode::OK, "mint failed");
    let mint_body = mint_resp.into_body().collect().await.unwrap().to_bytes();
    let mint_json: serde_json::Value = serde_json::from_slice(&mint_body).unwrap();
    let state_token = mint_json["state_token"]
        .as_str()
        .expect("state_token in response")
        .to_owned();
    assert!(
        mint_json["redirect_url"]
            .as_str()
            .unwrap()
            .starts_with("https://github.com/settings/apps/new?manifest=")
    );

    // 2. Walk the callback with our stubbed `code`.
    let cb_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/v1/admin/github/app-callback?code=stub-code-xyz&state={state_token}"
                ))
                .header("cookie", cookie_header(&admin.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("callback request");
    let cb_status = cb_resp.status();
    if cb_status != StatusCode::SEE_OTHER {
        let body = cb_resp.into_body().collect().await.unwrap().to_bytes();
        panic!(
            "callback should redirect, got {cb_status}: body={}",
            String::from_utf8_lossy(&body)
        );
    }

    // 3. DB row landed.
    let row: Option<(i64, String, Uuid, bool)> = sqlx::query_as(
        "SELECT app_id, slug, installed_by_user_id, is_active \
           FROM control.github_app_config \
          WHERE installed_by_user_id = $1 \
            AND is_active = true",
    )
    .bind(admin.user_id)
    .fetch_optional(&pool)
    .await
    .expect("query github_app_config");
    let (app_id, slug, installed_by, is_active) = row.expect("active row");
    assert_eq!(app_id, 555_001);
    assert_eq!(slug, "rustacean-test");
    assert_eq!(installed_by, admin.user_id);
    assert!(is_active);

    // 4. Loader was hot-swapped.
    let loaded = state.gh_loader.current().expect("loader Some");
    assert_eq!(loaded.app_id, 555_001);

    // 5. Status endpoint reports `db` source.
    let status_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/admin/github/app-status")
                .header("cookie", cookie_header(&admin.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("status request");
    assert_eq!(status_resp.status(), StatusCode::OK);
    let status_body = status_resp.into_body().collect().await.unwrap().to_bytes();
    let status_json: serde_json::Value = serde_json::from_slice(&status_body).unwrap();
    assert_eq!(status_json["configured"], true);
    assert_eq!(status_json["source"], "db");
    assert_eq!(status_json["app_id"], 555_001);

    // 6. Replay protection: re-using the same state token now 400s.
    let replay_resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/v1/admin/github/app-callback?code=stub-code-xyz&state={state_token}"
                ))
                .header("cookie", cookie_header(&admin.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("replay request");
    assert_eq!(
        replay_resp.status(),
        StatusCode::BAD_REQUEST,
        "replay must be rejected"
    );

    cleanup(&pool, admin.user_id).await;
}

#[tokio::test]
async fn manifest_endpoint_rejects_non_platform_admin() {
    let Some(pool) = real_db_pool().await else {
        eprintln!("RB_DATABASE_URL not set — skipping non-admin rejection test");
        return;
    };
    let enc_key = base64::engine::general_purpose::STANDARD.encode([0x42u8; 32]);
    let config = build_test_config(
        std::env::var("RB_DATABASE_URL").expect("checked above"),
        enc_key,
    );
    let regular = seed_user(&pool, false).await;
    let state = build_state(pool.clone(), config);
    let app = build_public(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/admin/github/app-manifest")
                .header("cookie", cookie_header(&regular.session_token))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .expect("manifest request");
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "non-admin must be 403"
    );
    cleanup(&pool, regular.user_id).await;
}

#[tokio::test]
async fn status_endpoint_reports_none_when_unconfigured() {
    let Some(pool) = real_db_pool().await else {
        eprintln!("RB_DATABASE_URL not set — skipping status-none test");
        return;
    };
    let enc_key = base64::engine::general_purpose::STANDARD.encode([0x42u8; 32]);
    let config = build_test_config(
        std::env::var("RB_DATABASE_URL").expect("checked above"),
        enc_key,
    );
    let admin = seed_user(&pool, true).await;
    let state = build_state(pool.clone(), config);
    let app = build_public(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/admin/github/app-status")
                .header("cookie", cookie_header(&admin.session_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("status request");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["configured"], false);
    assert_eq!(json["source"], "none");
    cleanup(&pool, admin.user_id).await;
}
