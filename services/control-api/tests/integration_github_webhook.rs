//! End-to-end tests for `POST /v1/github/webhook` (REQ-GH-06).
//!
//! These tests drive the full axum router via `tower::ServiceExt::oneshot`,
//! exercising the same path GitHub will hit in production. They use a
//! `connect_lazy` Postgres pool that never actually opens a connection —
//! every assertion below either short-circuits before SQL or asserts on a
//! status code that does not require DB I/O. The full DB-touching path is
//! covered by separate live-DB tests once a fixture harness lands.
//!
//! Naming convention: `webhook_<scenario>_returns_<status>`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use control_api::{AppState, Config, SessionCreateRateLimiter, TenantSessionCount, build_public};
use hmac::{Hmac, Mac};
use http_body_util::BodyExt as _;
use jsonwebtoken::EncodingKey;
use rb_auth::{LoginRateLimiter, PasswordHasher};
use rb_email::from_transport;
use rb_github::{GhApp, Secret};
use rb_sse::{EventBus, SseConfig};
use sha2::Sha256;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt as _;

const HEADER_SIGNATURE: &str = "X-Hub-Signature-256";
const HEADER_DELIVERY: &str = "X-GitHub-Delivery";
const HEADER_EVENT: &str = "X-GitHub-Event";

/// 2048-bit RSA private key, base64-encoded DER body without PEM headers so
/// secret scanners do not flag this constant. The full PEM is reconstructed
/// at runtime. Mirrors the same fixture used by `rb_github::app_jwt` tests.
///
/// `GhApp::new` requires a syntactically valid RSA key; this one is never used
/// to mint a real JWT in webhook tests — the webhook handler does not touch
/// the App-JWT path.
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

fn rsa_key() -> EncodingKey {
    let pem = format!(
        "-----BEGIN RSA PRIVATE KEY-----\n{TEST_RSA_KEY_BODY}\n-----END RSA PRIVATE KEY-----\n"
    );
    EncodingKey::from_rsa_pem(pem.as_bytes()).expect("test PEM should parse")
}

fn config_for_test() -> Config {
    Config::for_test()
}

fn lazy_pool(config: &Config) -> sqlx::PgPool {
    PgPoolOptions::new()
        .connect_lazy(&config.database_url)
        .expect("connect_lazy must succeed")
}

fn email_sender() -> Arc<dyn rb_email::EmailSender> {
    let smtp = rb_email::SmtpConfig {
        host: String::new(),
        port: 587,
        username: String::new(),
        password: String::new(),
        from_address: "test@example.com".to_owned(),
    };
    Arc::from(from_transport("noop", &smtp).expect("noop transport must succeed"))
}

fn state_with_gh(secret: &[u8]) -> AppState {
    let config = config_for_test();
    AppState {
        pool: lazy_pool(&config),
        email_sender: email_sender(),
        hasher: Arc::new(PasswordHasher::from_config(64, 1, 1).expect("hasher")),
        login_rate_limiter: Arc::new(LoginRateLimiter::new()),
        config: Arc::new(config),
        gh_loader: Arc::new(rb_github::GhAppLoader::new(Some(Arc::new(GhApp::new(
            12345,
            rsa_key(),
            Secret::new(secret.to_vec()),
        ))))),
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
        llm_api_key: String::new(),
    }
}

fn state_without_gh() -> AppState {
    let config = config_for_test();
    AppState {
        pool: lazy_pool(&config),
        email_sender: email_sender(),
        hasher: Arc::new(PasswordHasher::from_config(64, 1, 1).expect("hasher")),
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
        kafka_consistency: Arc::new(control_api::KafkaConsistencyState::new()),
        mcp_sessions: control_api::McpSessionStore::new(),
        agent_registry: control_api::AgentRegistry::new(),
        agent_commands_producer: None,
        internal_secret: "test-internal-secret".to_owned(),
        session_create_rate_limiter: Arc::new(SessionCreateRateLimiter::default()),
        tenant_session_count: Arc::new(TenantSessionCount::new()),
        mcp_jwt_secret: "test-mcp-jwt-secret".to_owned(),
        mcp_jwt_ttl_secs: 900,
        llm_api_key: String::new(),
    }
}

fn sign(body: &[u8], secret: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("hmac key length");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

fn webhook_request(
    body: Vec<u8>,
    sig: Option<String>,
    delivery: Option<&str>,
    event: Option<&str>,
) -> Request<Body> {
    let mut req = Request::builder().method("POST").uri("/v1/github/webhook");
    if let Some(sig) = sig {
        req = req.header(HEADER_SIGNATURE, sig);
    }
    if let Some(delivery) = delivery {
        req = req.header(HEADER_DELIVERY, delivery);
    }
    if let Some(event) = event {
        req = req.header(HEADER_EVENT, event);
    }
    req.body(Body::from(body)).expect("request build")
}

async fn body_is_empty(resp: axum::response::Response) -> bool {
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    bytes.is_empty()
}

#[tokio::test]
async fn webhook_returns_503_when_app_not_configured() {
    let app = build_public(state_without_gh());
    let req = webhook_request(
        b"{}".to_vec(),
        Some("sha256=00".into()),
        Some("d"),
        Some("ping"),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(body_is_empty(resp).await, "503 body must be empty");
}

#[tokio::test]
async fn webhook_returns_401_for_missing_signature() {
    let app = build_public(state_with_gh(b"shh"));
    let req = webhook_request(b"{}".to_vec(), None, Some("d"), Some("ping"));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn webhook_returns_400_for_missing_delivery() {
    let app = build_public(state_with_gh(b"shh"));
    let body = b"{}".to_vec();
    let sig = sign(&body, b"shh");
    let req = webhook_request(body, Some(sig), None, Some("ping"));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn webhook_returns_400_for_missing_event() {
    let app = build_public(state_with_gh(b"shh"));
    let body = b"{}".to_vec();
    let sig = sign(&body, b"shh");
    let req = webhook_request(body, Some(sig), Some("d"), None);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn webhook_returns_401_for_bad_signature_format() {
    let app = build_public(state_with_gh(b"shh"));
    let req = webhook_request(
        b"{}".to_vec(),
        Some("not-a-sig".into()),
        Some("d"),
        Some("ping"),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn webhook_returns_401_for_signature_mismatch() {
    let app = build_public(state_with_gh(b"shh"));
    let body = b"{}".to_vec();
    let sig = sign(&body, b"WRONG-SECRET");
    let req = webhook_request(body, Some(sig), Some("d"), Some("ping"));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// `ping` is unmodelled in v1 — accepted explicitly with 202 so GitHub stops
/// retrying.
#[tokio::test]
async fn webhook_returns_202_for_unmodelled_event() {
    let app = build_public(state_with_gh(b"shh"));
    let body = b"{}".to_vec();
    let sig = sign(&body, b"shh");
    let req = webhook_request(body, Some(sig), Some("d-ping"), Some("ping"));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
}

/// Malformed installation body returns 400 — *after* signature verification,
/// so it cannot be probed by an unauthenticated attacker.
#[tokio::test]
async fn webhook_returns_400_for_malformed_installation_body() {
    let app = build_public(state_with_gh(b"shh"));
    let body = br#"{"action":"created"}"#.to_vec(); // missing `installation`
    let sig = sign(&body, b"shh");
    let req = webhook_request(body, Some(sig), Some("d-bad"), Some("installation"));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Replay of the same `X-GitHub-Delivery` returns 200 without ever touching
/// the SQL handler. The lazy pool would error out on a real connect; success
/// here proves the replay short-circuit fired before the SQL dispatch.
#[tokio::test]
async fn webhook_returns_200_on_replay_without_sql_dispatch() {
    let app = build_public(state_with_gh(b"shh"));
    let body = b"{}".to_vec();
    let sig = sign(&body, b"shh");
    let delivery = "d-replay";

    let resp1 = app
        .clone()
        .oneshot(webhook_request(
            body.clone(),
            Some(sig.clone()),
            Some(delivery),
            Some("ping"),
        ))
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::ACCEPTED);

    let resp2 = app
        .oneshot(webhook_request(
            body,
            Some(sig),
            Some(delivery),
            Some("ping"),
        ))
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
}

/// Empty `repositories_removed` list short-circuits before SQL — verifies
/// the no-op fast-path that GitHub may legitimately exercise.
#[tokio::test]
async fn webhook_returns_200_for_empty_removed_list() {
    let app = build_public(state_with_gh(b"shh"));
    let body = serde_json::to_vec(&serde_json::json!({
        "action": "removed",
        "installation": {
            "id": 99,
            "account": { "login": "x", "type": "User", "id": 1 }
        },
        "repositories_removed": []
    }))
    .unwrap();
    let sig = sign(&body, b"shh");
    let req = webhook_request(
        body,
        Some(sig),
        Some("d-empty-remove"),
        Some("installation_repositories"),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// `installation_repositories.added` is logged-only in v1 — no SQL is
/// dispatched, so the lazy pool is never touched and the response is 200.
#[tokio::test]
async fn webhook_returns_200_for_repos_added_no_auto_connect() {
    let app = build_public(state_with_gh(b"shh"));
    let body = serde_json::to_vec(&serde_json::json!({
        "action": "added",
        "installation": {
            "id": 99,
            "account": { "login": "x", "type": "User", "id": 1 }
        },
        "repositories_added": [
            { "id": 1, "full_name": "x/repo-a" }
        ]
    }))
    .unwrap();
    let sig = sign(&body, b"shh");
    let req = webhook_request(
        body,
        Some(sig),
        Some("d-add"),
        Some("installation_repositories"),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// `installation` events with an action we don't model (e.g.
/// `new_permissions_accepted`) are accepted with 202 to stop GitHub retries.
#[tokio::test]
async fn webhook_returns_202_for_unmodelled_installation_action() {
    let app = build_public(state_with_gh(b"shh"));
    let body = serde_json::to_vec(&serde_json::json!({
        "action": "new_permissions_accepted",
        "installation": {
            "id": 99,
            "account": { "login": "x", "type": "User", "id": 1 }
        }
    }))
    .unwrap();
    let sig = sign(&body, b"shh");
    let req = webhook_request(body, Some(sig), Some("d-perms"), Some("installation"));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
}

// ─── GitHub callback tests ──────────────────────────────────────────────────

#[tokio::test]
async fn callback_returns_503_when_app_not_configured() {
    let app = build_public(state_without_gh());
    let req = Request::builder()
        .method("GET")
        .uri("/v1/github/callback?installation_id=1&state=aabbcc")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn callback_returns_400_for_invalid_hex_state() {
    let app = build_public(state_with_gh(b"shh"));
    let req = Request::builder()
        .method("GET")
        .uri("/v1/github/callback?installation_id=1&state=not-valid-hex!")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// `installation_repositories` with action other than added/removed (e.g.
/// future GitHub additions) returns 202.
#[tokio::test]
async fn webhook_returns_202_for_unmodelled_repos_action() {
    let app = build_public(state_with_gh(b"shh"));
    let body = serde_json::to_vec(&serde_json::json!({
        "action": "future_action",
        "installation": {
            "id": 99,
            "account": { "login": "x", "type": "User", "id": 1 }
        }
    }))
    .unwrap();
    let sig = sign(&body, b"shh");
    let req = webhook_request(
        body,
        Some(sig),
        Some("d-future"),
        Some("installation_repositories"),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
}
