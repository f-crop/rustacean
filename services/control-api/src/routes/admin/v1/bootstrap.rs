//! `POST /api/admin/v1/bootstrap/admin` — create the first platform admin.
//!
//! Refuses with `409 Conflict` if any user exists in `control.users`.
//! Succeeds only once; all subsequent calls are rejected regardless of
//! the supplied credentials.

use axum::{Extension, Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::admin_auth::{AdminActor, AdminRequestId},
    routes::admin::v1::write_audit_row,
    state::AppState,
};

// ---------------------------------------------------------------------------
// DTO
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct BootstrapAdminReq {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct BootstrapAdminResp {
    pub user_id: Uuid,
    pub email: String,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// `POST /api/admin/v1/bootstrap/admin`
///
/// Creates the first platform-admin user. Returns `409` if any user already
/// exists (idempotent in the sense that re-running on an already-bootstrapped
/// stack is safe — it simply refuses).
///
/// Writes one audit row on every code path (ADR-012 §S1.6.1).
pub async fn bootstrap_admin(
    State(state): State<AppState>,
    Extension(AdminActor(actor)): Extension<AdminActor>,
    Extension(AdminRequestId(request_id)): Extension<AdminRequestId>,
    Json(body): Json<BootstrapAdminReq>,
) -> Result<impl IntoResponse, AppError> {
    // Validate input before any DB touch.
    let email = body.email.trim();
    if email.is_empty() {
        return Err(AppError::InvalidEmail);
    }
    if body.password.len() < 12 {
        return Err(AppError::WeakPassword);
    }

    // Check zero-user precondition.
    let (user_count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM control.users")
        .fetch_one(&state.pool)
        .await?;

    if user_count > 0 {
        write_audit_row(
            &state.pool,
            &actor,
            "bootstrap.admin",
            None,
            None,
            request_id,
            None,
            None,
            &serde_json::json!({"email": email, "existing_users": user_count}),
            "denied",
            Some("users_already_exist"),
        )
        .await;
        return Err(AppError::AdminBootstrapConflict);
    }

    let user_id = Uuid::new_v4();
    let hash = state.hasher.hash(&body.password)?;

    sqlx::query(
        "INSERT INTO control.users \
         (id, email, password_hash, email_verified_at, is_platform_admin, status) \
         VALUES ($1, $2, $3, NOW(), true, 'active')",
    )
    .bind(user_id)
    .bind(email)
    .bind(hash)
    .execute(&state.pool)
    .await?;

    write_audit_row(
        &state.pool,
        &actor,
        "bootstrap.admin",
        None,
        Some(user_id),
        request_id,
        None,
        None,
        &serde_json::json!({"email": email}),
        "ok",
        None,
    )
    .await;

    tracing::info!(
        user_id = %user_id,
        actor = %actor,
        "bootstrap admin user created"
    );

    Ok((
        StatusCode::CREATED,
        Json(BootstrapAdminResp {
            user_id,
            email: email.to_owned(),
        }),
    ))
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_resp_serializes() {
        let uid = Uuid::new_v4();
        let resp = BootstrapAdminResp {
            user_id: uid,
            email: "admin@example.com".to_owned(),
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["email"], "admin@example.com");
        assert_eq!(v["user_id"], uid.to_string());
    }
}
