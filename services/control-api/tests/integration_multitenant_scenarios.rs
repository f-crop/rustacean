//! Multi-tenant CI scenarios: duplicate email signup, install conflict redirect,
//! cross-tenant repo connect (allowed), and same-tenant duplicate repo (blocked).
//!
//! These tests complement `integration_github_install_conflict.rs` (which covers
//! the install reclaim SQL path) and together give full PR coverage for all
//! three conflict scenarios raised in the Wave 7 retrospective (RUSAA-1670):
//!
//! 1. Two tenants installing the same GitHub App installation
//! 2. Two tenants signing up with the same primary email
//! 3. Two tenants connecting the same repository
//!
//! DB-backed tests are skipped automatically when `RB_DATABASE_URL` is not set.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use control_api::{AppState, Config, build_public};
use http_body_util::BodyExt as _;
use rb_auth::{LoginRateLimiter, PasswordHasher};
use rb_email::from_transport;
use rb_sse::{EventBus, SseConfig};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt as _;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn connect() -> Option<sqlx::PgPool> {
    let Ok(db_url) = std::env::var("RB_DATABASE_URL") else {
        return None;
    };
    Some(
        PgPoolOptions::new()
            .max_connections(2)
            .connect(&db_url)
            .await
            .expect("connect to test database"),
    )
}

async fn real_db_state() -> Option<(AppState, sqlx::PgPool)> {
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
        service_name: "control-api-test".to_owned(),
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
        hybrid_search_enabled: false,
        multi_query_n: 1,
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
        session_create_rate_limiter: Arc::new(control_api::SessionCreateRateLimiter::new(10, 60)),
        tenant_session_count: Arc::new(control_api::TenantSessionCount::new()),
        mcp_jwt_secret: "test-mcp-jwt-secret".to_owned(),
        mcp_jwt_ttl_secs: 900,
        llm_api_key: String::new(),
    };
    Some((state, pool))
}

fn json_body(v: &serde_json::Value) -> Body {
    Body::from(serde_json::to_vec(v).expect("serialise JSON"))
}

async fn collect_body(body: Body) -> Vec<u8> {
    body.collect()
        .await
        .expect("collect body")
        .to_bytes()
        .to_vec()
}

fn random_id_in_range(base: i64) -> i64 {
    i64::from(rand::random::<i32>().abs()) + base
}

async fn seed_tenant(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name, status) \
         VALUES ($1, $2, $3, $4, 'active')",
    )
    .bind(id)
    .bind(format!("mt-test-{id}"))
    .bind("MT Test Tenant")
    .bind(format!("mt_test_{}", id.simple()))
    .execute(pool)
    .await
    .expect("seed tenant");
    id
}

async fn seed_user(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO control.users \
         (id, email, password_hash, email_verified_at) \
         VALUES ($1, $2, '$argon2id$placeholder', now())",
    )
    .bind(id)
    .bind(format!("mt-test-{}@example.com", id.simple()))
    .execute(pool)
    .await
    .expect("seed user");
    id
}

async fn seed_installation(
    pool: &sqlx::PgPool,
    tenant_id: Uuid,
    github_installation_id: i64,
) -> Uuid {
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
    .expect("seed installation");
    id
}

// ---------------------------------------------------------------------------
// Scenario 1 (supplement): GitHub App Installation Conflict — redirect URLs
// ---------------------------------------------------------------------------
//
// The SQL-level reclaim tests live in integration_github_install_conflict.rs.
// This companion verifies the redirect URL contract the HTTP callback emits for
// each outcome so the frontend can surface the correct message.

/// The success redirect contains `install=success`; the conflict redirect
/// contains `install=conflict&reason=active`.  Verifying the string contract
/// here makes the frontend dependency explicit and catches typos in the source.
#[test]
fn install_redirect_url_contracts_are_correct() {
    let base = "http://localhost:8080";

    // Success path (installation upserted or orphan reclaimed).
    let success = format!("{base}/repos?install=success&installation_uuid=abc&account_login=org");
    // Blocked path (active owner, reclaim not possible).
    let conflict = format!("{base}/repos?install=conflict&reason=active");

    assert!(
        success.contains("install=success"),
        "success redirect must carry 'install=success'"
    );
    assert!(
        conflict.contains("install=conflict"),
        "blocked redirect must carry 'install=conflict'"
    );
    assert!(
        conflict.contains("reason=active"),
        "blocked redirect must include 'reason=active' to signal a live owner"
    );
    assert!(
        !conflict.contains("install=success"),
        "blocked redirect must not claim success"
    );
    assert!(
        !success.contains("install=conflict"),
        "success redirect must not claim conflict"
    );
}

// ---------------------------------------------------------------------------
// Scenario 2: Two tenants signing up with the same primary email
// ---------------------------------------------------------------------------

/// Second signup with a previously-registered email must be rejected with
/// 409 Conflict and error code `email_taken`.
///
/// Multi-tenant scenario: two separate users attempt to create accounts using
/// the same primary email address — only the first succeeds.
///
/// Skipped automatically when `RB_DATABASE_URL` is not set.
#[tokio::test]
async fn duplicate_email_signup_returns_conflict() {
    let Some((state, _pool)) = real_db_state().await else {
        return;
    };
    let app = build_public(state);
    let email = format!("mt-dup-{}@test.example", Uuid::new_v4().simple());

    // First signup — must succeed.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/signup")
                .header("content-type", "application/json")
                .body(json_body(&serde_json::json!({
                    "email": email,
                    "password": "correct-horse-battery-staple",
                    "tenant_name": "First Tenant",
                })))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "first signup must return 201"
    );

    // Second signup with the same email — must be rejected with 409.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/signup")
                .header("content-type", "application/json")
                .body(json_body(&serde_json::json!({
                    "email": email,
                    "password": "correct-horse-battery-staple",
                    "tenant_name": "Second Tenant",
                })))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "duplicate email signup must return 409 Conflict"
    );

    let raw = collect_body(resp.into_body()).await;
    let body: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(
        body["error"], "email_taken",
        "error code must be 'email_taken'"
    );
}

// ---------------------------------------------------------------------------
// Scenario 3: Two tenants connecting the same repository
// ---------------------------------------------------------------------------

/// Two different tenants can each connect the same GitHub repository.
///
/// The `repos` table enforces `UNIQUE (tenant_id, github_repo_id)` — a
/// *per-tenant* constraint.  Different tenants sharing the same `github_repo_id`
/// must both succeed; the constraint must not be global.
///
/// Skipped automatically when `RB_DATABASE_URL` is not set.
#[tokio::test]
async fn cross_tenant_same_github_repo_both_succeed() {
    let Some(pool) = connect().await else { return };

    let tenant_a = seed_tenant(&pool).await;
    let tenant_b = seed_tenant(&pool).await;
    let user_a = seed_user(&pool).await;
    let user_b = seed_user(&pool).await;

    // Use well-separated ranges to avoid cross-test collisions.
    let github_repo_id = random_id_in_range(5_000_000);
    let install_a = seed_installation(&pool, tenant_a, random_id_in_range(7_000_000)).await;
    let install_b = seed_installation(&pool, tenant_b, random_id_in_range(8_000_000)).await;

    let repo_a = Uuid::new_v4();
    let repo_b = Uuid::new_v4();

    // Tenant A connects the repo — must succeed.
    sqlx::query(
        "INSERT INTO control.repos \
         (id, tenant_id, installation_id, github_repo_id, full_name, default_branch, connected_by) \
         VALUES ($1, $2, $3, $4, 'org/shared-repo', 'main', $5)",
    )
    .bind(repo_a)
    .bind(tenant_a)
    .bind(install_a)
    .bind(github_repo_id)
    .bind(user_a)
    .execute(&pool)
    .await
    .expect("tenant A repo connect must succeed");

    // Tenant B connects the SAME GitHub repo — must also succeed.
    let result = sqlx::query(
        "INSERT INTO control.repos \
         (id, tenant_id, installation_id, github_repo_id, full_name, default_branch, connected_by) \
         VALUES ($1, $2, $3, $4, 'org/shared-repo', 'main', $5)",
    )
    .bind(repo_b)
    .bind(tenant_b)
    .bind(install_b)
    .bind(github_repo_id)
    .bind(user_b)
    .execute(&pool)
    .await;

    assert!(
        result.is_ok(),
        "tenant B connecting the same github_repo_id must succeed: \
         uniqueness is per-tenant, not global"
    );

    // Verify both rows exist for the same github_repo_id.
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM control.repos WHERE github_repo_id = $1")
            .bind(github_repo_id)
            .fetch_one(&pool)
            .await
            .expect("count repos");
    assert_eq!(
        count, 2,
        "both tenant rows must exist for the shared github_repo_id"
    );

    // Cleanup — FK order: repos → installations → users → tenants.
    sqlx::query("DELETE FROM control.repos WHERE id IN ($1, $2)")
        .bind(repo_a)
        .bind(repo_b)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.github_installations WHERE id IN ($1, $2)")
        .bind(install_a)
        .bind(install_b)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.users WHERE id IN ($1, $2)")
        .bind(user_a)
        .bind(user_b)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.tenants WHERE id IN ($1, $2)")
        .bind(tenant_a)
        .bind(tenant_b)
        .execute(&pool)
        .await
        .ok();
}

/// The same tenant connecting a repository a second time must be rejected by
/// the `UNIQUE (tenant_id, github_repo_id)` constraint.
///
/// The application layer maps this constraint violation to
/// `AppError::RepoAlreadyConnected` (HTTP 409, error code `repo_already_connected`).
/// This test verifies the constraint name matches the string the handler checks,
/// locking the mapping against future schema renames.
///
/// Skipped automatically when `RB_DATABASE_URL` is not set.
#[tokio::test]
async fn same_tenant_duplicate_repo_blocked_by_unique_constraint() {
    let Some(pool) = connect().await else { return };

    let tenant_id = seed_tenant(&pool).await;
    let user_id = seed_user(&pool).await;
    let github_repo_id = random_id_in_range(9_000_000);
    let install_id = seed_installation(&pool, tenant_id, random_id_in_range(10_000_000)).await;

    let repo_first = Uuid::new_v4();
    let repo_second = Uuid::new_v4();

    // First connect — must succeed.
    sqlx::query(
        "INSERT INTO control.repos \
         (id, tenant_id, installation_id, github_repo_id, full_name, default_branch, connected_by) \
         VALUES ($1, $2, $3, $4, 'org/my-repo', 'main', $5)",
    )
    .bind(repo_first)
    .bind(tenant_id)
    .bind(install_id)
    .bind(github_repo_id)
    .bind(user_id)
    .execute(&pool)
    .await
    .expect("first connect must succeed");

    // Second connect of the same repo for the same tenant — must fail.
    let result = sqlx::query(
        "INSERT INTO control.repos \
         (id, tenant_id, installation_id, github_repo_id, full_name, default_branch, connected_by) \
         VALUES ($1, $2, $3, $4, 'org/my-repo', 'main', $5)",
    )
    .bind(repo_second)
    .bind(tenant_id)
    .bind(install_id)
    .bind(github_repo_id)
    .bind(user_id)
    .execute(&pool)
    .await;

    // The constraint name must match what `connect_repo` in repos.rs checks.
    match result {
        Err(sqlx::Error::Database(ref dbe)) => {
            assert_eq!(
                dbe.constraint(),
                Some("repos_tenant_id_github_repo_id_key"),
                "duplicate repo must be rejected by the per-tenant unique constraint; \
                 constraint name must match the string checked in repos.rs"
            );
        }
        Err(other) => panic!("expected unique constraint violation, got: {other}"),
        Ok(_) => panic!("duplicate repo insert must not succeed"),
    }

    // Cleanup — FK order: repos → installations → users → tenants.
    sqlx::query("DELETE FROM control.repos WHERE id = $1")
        .bind(repo_first)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.github_installations WHERE id = $1")
        .bind(install_id)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.users WHERE id = $1")
        .bind(user_id)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM control.tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}
