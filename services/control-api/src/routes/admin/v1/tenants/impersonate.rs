use axum::{
    Extension, Json,
    extract::{Path, State},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use rand::Rng as _;
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::admin_auth::{AdminActor, AdminRequestId},
    routes::admin::v1::write_audit_row,
    state::AppState,
};

/// Maximum impersonation session lifetime in seconds (15 minutes, server-enforced ceiling).
pub(super) const IMPERSONATION_MAX_SECS: i64 = 900;

fn default_impersonation_secs() -> i64 {
    IMPERSONATION_MAX_SECS
}

#[derive(Debug, Deserialize)]
pub struct ImpersonateReq {
    /// User to impersonate (must be a member of `tenant_id`).
    pub user_id: Uuid,
    /// Requested session duration in seconds. Server-enforced ceiling: 900 s.
    #[serde(default = "default_impersonation_secs")]
    pub duration_secs: i64,
}

#[derive(Debug, Serialize)]
pub struct ImpersonateResp {
    pub token: String,
    pub expires_at: String,
    pub user_id: Uuid,
    pub tenant_id: Uuid,
}

/// JWT claims for an impersonation session (ADR-012 §S1.2).
#[derive(Debug, Serialize, Deserialize)]
struct ImpersonationClaims {
    /// Subject: impersonated user ID.
    sub: String,
    /// Tenant ID.
    tid: String,
    /// Admin actor who minted this token.
    imp: String,
    /// One-time nonce (ensures every mint produces a distinct token).
    nonce: String,
    /// Unix timestamp expiry — server-enforced ceiling of `now + 900 s`.
    exp: usize,
    /// Token type discriminator.
    typ: String,
}

pub async fn impersonate(
    State(state): State<AppState>,
    Extension(AdminActor(actor)): Extension<AdminActor>,
    Extension(AdminRequestId(request_id)): Extension<AdminRequestId>,
    Path(tenant_id): Path<Uuid>,
    Json(body): Json<ImpersonateReq>,
) -> Result<Response, AppError> {
    // Clamp duration to the server-enforced ceiling [1, 900].
    let duration_secs = body.duration_secs.clamp(1, IMPERSONATION_MAX_SECS);

    // Verify the user is a member of the requested tenant.
    let member_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(\
            SELECT 1 FROM control.tenant_members \
            WHERE tenant_id = $1 AND user_id = $2\
         )",
    )
    .bind(tenant_id)
    .bind(body.user_id)
    .fetch_one(&state.pool)
    .await?;

    if !member_exists {
        write_audit_row(
            &state.pool,
            &actor,
            "tenant.impersonate.start",
            Some(tenant_id),
            Some(body.user_id),
            request_id,
            None,
            None,
            &serde_json::json!({"duration_secs": duration_secs}),
            "denied",
            Some("user_not_tenant_member"),
        )
        .await;
        return Err(AppError::NotAMember);
    }

    let admin_token = state.config.admin_token.as_deref().unwrap_or("");

    let nonce: String = {
        let bytes: [u8; 16] = rand::rng().random();
        hex::encode(bytes)
    };

    let exp = Utc::now().timestamp().saturating_add(duration_secs);

    let claims = ImpersonationClaims {
        sub: body.user_id.to_string(),
        tid: tenant_id.to_string(),
        imp: actor.clone(),
        nonce: nonce.clone(),
        exp: usize::try_from(exp).unwrap_or(0),
        typ: "imp".to_owned(),
    };

    // Signing key: SHA-256(admin_token || ":imp:" || nonce).
    // Rotating admin_token invalidates all outstanding impersonation JWTs
    // because the derived key changes (ADR-012 §S1.2 threat model).
    let key_material = format!("{admin_token}:imp:{nonce}");
    let key_bytes = sha2::Sha256::digest(key_material.as_bytes());
    let encoding_key = EncodingKey::from_secret(&key_bytes);

    let token = jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &encoding_key)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("JWT encode failed: {e}")))?;

    let expires_at = chrono::DateTime::from_timestamp(exp, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();

    write_audit_row(
        &state.pool,
        &actor,
        "tenant.impersonate.start",
        Some(tenant_id),
        Some(body.user_id),
        request_id,
        None,
        None,
        &serde_json::json!({
            "duration_secs": duration_secs,
            "expires_at": expires_at,
        }),
        "ok",
        None,
    )
    .await;

    tracing::info!(
        actor = %actor,
        %tenant_id,
        user_id = %body.user_id,
        duration_secs,
        "impersonation token minted"
    );

    Ok(Json(ImpersonateResp {
        token,
        expires_at,
        user_id: body.user_id,
        tenant_id,
    })
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impersonation_max_secs_is_900() {
        assert_eq!(IMPERSONATION_MAX_SECS, 900);
    }

    #[test]
    fn duration_clamped_to_ceiling() {
        let clamped = 9999_i64.min(IMPERSONATION_MAX_SECS);
        assert_eq!(clamped, 900);
    }
}
