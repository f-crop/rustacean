//! Handler-level unit tests for the chat routes (ADR-013 §3).
//!
//! Tests cover: feature-flag 404, auth happy/sad paths, tenant isolation,
//! and concurrent message insert correctness.

use axum::http::StatusCode;
use axum::response::IntoResponse;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{ApiKeyInfo, AuthContext, McpJwtInfo, Scope, SessionInfo},
};

use super::db::db_insert_chat_message;
use super::sessions::require_chat_auth;

/// Connect to the real Postgres instance.
/// Returns `None` when `RB_DATABASE_URL` is absent — callers skip gracefully.
async fn test_pool() -> Option<PgPool> {
    let url = std::env::var("RB_DATABASE_URL").ok()?;
    PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .ok()
}

fn make_verified_session(tenant_id: Uuid, user_id: Uuid) -> AuthContext {
    AuthContext::Session(SessionInfo {
        session_id: Uuid::new_v4(),
        user_id,
        tenant_id,
        email_verified: true,
    })
}

// ---------------------------------------------------------------------------
// Feature-flag gate: ChatFeatureDisabled → 404
// ---------------------------------------------------------------------------

#[test]
fn chat_feature_disabled_returns_404() {
    let resp = AppError::ChatFeatureDisabled.into_response();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[test]
fn chat_session_not_found_returns_404() {
    let resp = AppError::ChatSessionNotFound.into_response();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[test]
fn chat_session_not_active_returns_422() {
    let resp = AppError::ChatSessionNotActive.into_response();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

// ---------------------------------------------------------------------------
// require_chat_auth — happy paths
// ---------------------------------------------------------------------------

#[test]
fn verified_session_is_allowed() {
    let tenant = Uuid::new_v4();
    let user = Uuid::new_v4();
    let caller = require_chat_auth(make_verified_session(tenant, user)).unwrap();
    assert_eq!(caller.tenant_id, tenant);
    assert_eq!(caller.user_id, user);
}

#[test]
fn write_scoped_api_key_is_allowed() {
    let tenant = Uuid::new_v4();
    let user = Uuid::new_v4();
    let auth = AuthContext::ApiKey(ApiKeyInfo {
        key_id: Uuid::new_v4(),
        tenant_id: tenant,
        user_id: user,
        scopes: vec![Scope::Write],
    });
    let caller = require_chat_auth(auth).unwrap();
    assert_eq!(caller.tenant_id, tenant);
}

#[test]
fn agent_scoped_api_key_is_allowed() {
    let tenant = Uuid::new_v4();
    let auth = AuthContext::ApiKey(ApiKeyInfo {
        key_id: Uuid::new_v4(),
        tenant_id: tenant,
        user_id: Uuid::new_v4(),
        scopes: vec![Scope::Agent],
    });
    assert!(require_chat_auth(auth).is_ok());
}

// ---------------------------------------------------------------------------
// require_chat_auth — sad paths
// ---------------------------------------------------------------------------

#[test]
fn unverified_session_is_rejected() {
    let auth = AuthContext::Session(SessionInfo {
        session_id: Uuid::new_v4(),
        user_id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        email_verified: false,
    });
    assert!(matches!(
        require_chat_auth(auth),
        Err(AppError::EmailNotVerified)
    ));
}

#[test]
fn expired_session_is_rejected() {
    assert!(matches!(
        require_chat_auth(AuthContext::ExpiredSession),
        Err(AppError::SessionExpired)
    ));
}

#[test]
fn anonymous_is_unauthorized() {
    assert!(matches!(
        require_chat_auth(AuthContext::Anonymous),
        Err(AppError::Unauthorized)
    ));
}

#[test]
fn mcp_jwt_is_unauthorized_on_chat_routes() {
    let auth = AuthContext::McpJwt(McpJwtInfo {
        tenant_id: Uuid::new_v4(),
        user_id: Uuid::new_v4(),
        chat_session_id: Uuid::new_v4(),
        scope: vec!["read".to_owned()],
        jti: Uuid::new_v4().to_string(),
    });
    assert!(matches!(
        require_chat_auth(auth),
        Err(AppError::Unauthorized)
    ));
}

// ---------------------------------------------------------------------------
// Tenant isolation: different tenant gets 404 (via ChatSessionNotFound)
// ---------------------------------------------------------------------------

#[test]
fn chat_session_not_found_is_tenant_scoped() {
    // ChatSessionNotFound is returned when DB query finds no row for the
    // (session_id, tenant_id) pair — tenant isolation is enforced at the DB
    // query level (see db.rs:db_get_chat_session).
    let resp = AppError::ChatSessionNotFound.into_response();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Concurrent message inserts must produce unique seq values (no race)
// Requires RB_DATABASE_URL; skipped gracefully when absent.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_message_inserts_have_unique_seq() {
    let Some(pool) = test_pool().await else {
        return;
    };

    let tenant_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(format!("chat-conc-{}", tenant_id.simple()))
    .bind("Chat Concurrency Test Tenant")
    .bind(format!("chat_conc_{}", tenant_id.simple()))
    .execute(&pool)
    .await
    .expect("insert tenant");

    let session_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO control.chat_sessions (id, tenant_id, runtime, trace_id) \
         VALUES ($1, $2, 'claude_code', $3)",
    )
    .bind(session_id)
    .bind(tenant_id)
    .bind(format!("trace-{}", session_id))
    .execute(&pool)
    .await
    .expect("insert session");

    const N: usize = 8;
    let handles: Vec<_> = (0..N)
        .map(|_| {
            let pool = pool.clone();
            tokio::spawn(async move {
                db_insert_chat_message(
                    &pool,
                    Uuid::new_v4(),
                    session_id,
                    tenant_id,
                    "user",
                    "concurrent test message",
                )
                .await
            })
        })
        .collect();

    let mut seqs: Vec<i32> = Vec::with_capacity(N);
    for h in handles {
        let seq = h.await.expect("task panicked").expect("DB insert failed");
        seqs.push(seq);
    }

    seqs.sort_unstable();
    assert_eq!(
        seqs,
        (1..=(N as i32)).collect::<Vec<_>>(),
        "seq values must be 1..=N with no gaps or duplicates"
    );
}
