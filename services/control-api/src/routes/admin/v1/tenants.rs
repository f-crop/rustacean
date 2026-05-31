//! Admin tenant-management endpoints (ADR-012 §S1.2):
//!
//! - `POST /api/admin/v1/tenants/:id/rebind-gh-install`
//! - `POST /api/admin/v1/tenants/:id/impersonate`
//! - `POST /api/admin/v1/tenants/:id/force-delete`

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

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum impersonation session lifetime in seconds (15 minutes, server-enforced ceiling).
const IMPERSONATION_MAX_SECS: i64 = 900;

/// Confirm-token TTL for the two-phase force-delete (60 seconds).
const FORCE_DELETE_CONFIRM_TTL_SECS: i64 = 60;

// ---------------------------------------------------------------------------
// ─── rebind-gh-install ─────────────────────────────────────────────────────
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RebindGhInstallReq {
    /// Numeric GitHub App installation ID to rebind.
    pub github_installation_id: i64,
    /// When `true` the rebind proceeds even if the installation currently
    /// points at a different tenant. Requires `reason`.
    #[serde(default)]
    pub force: bool,
    /// Human-readable reason for the rebind (required when `force=true`).
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[allow(clippy::struct_field_names)]
pub struct RebindGhInstallResp {
    pub installation_id: Uuid,
    pub tenant_id: Uuid,
    pub previous_tenant_id: Option<Uuid>,
}

#[allow(clippy::too_many_lines)]
pub async fn rebind_gh_install(
    State(state): State<AppState>,
    Extension(AdminActor(actor)): Extension<AdminActor>,
    Extension(AdminRequestId(request_id)): Extension<AdminRequestId>,
    Path(tenant_id): Path<Uuid>,
    Json(body): Json<RebindGhInstallReq>,
) -> Result<Response, AppError> {
    if body.force && body.reason.as_deref().unwrap_or("").trim().is_empty() {
        write_audit_row(
            &state.pool,
            &actor,
            "tenant.rebind_gh",
            Some(tenant_id),
            None,
            request_id,
            None,
            None,
            &serde_json::json!({
                "github_installation_id": body.github_installation_id,
                "force": body.force,
                "reason": body.reason,
            }),
            "denied",
            Some("force_requires_reason"),
        )
        .await;
        return Err(AppError::InvalidInput);
    }

    // Check target tenant exists.
    let tenant_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM control.tenants WHERE id = $1 AND deleted_at IS NULL)",
    )
    .bind(tenant_id)
    .fetch_one(&state.pool)
    .await?;
    if !tenant_exists {
        write_audit_row(
            &state.pool,
            &actor,
            "tenant.rebind_gh",
            Some(tenant_id),
            None,
            request_id,
            None,
            None,
            &serde_json::json!({
                "github_installation_id": body.github_installation_id,
            }),
            "denied",
            Some("tenant_not_found"),
        )
        .await;
        return Err(AppError::NotFound);
    }

    // Look up the installation row.
    let row: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT id, tenant_id \
         FROM control.github_installations \
         WHERE github_installation_id = $1 AND deleted_at IS NULL",
    )
    .bind(body.github_installation_id)
    .fetch_optional(&state.pool)
    .await?;

    let Some((install_row_id, current_tenant_id)) = row else {
        write_audit_row(
            &state.pool,
            &actor,
            "tenant.rebind_gh",
            Some(tenant_id),
            None,
            request_id,
            None,
            None,
            &serde_json::json!({
                "github_installation_id": body.github_installation_id,
            }),
            "denied",
            Some("installation_not_found"),
        )
        .await;
        return Err(AppError::NotFound);
    };

    // Already bound to this tenant — idempotent no-op.
    if current_tenant_id == tenant_id {
        write_audit_row(
            &state.pool,
            &actor,
            "tenant.rebind_gh",
            Some(tenant_id),
            None,
            request_id,
            None,
            None,
            &serde_json::json!({
                "github_installation_id": body.github_installation_id,
                "note": "already_bound_to_tenant",
            }),
            "ok",
            None,
        )
        .await;
        return Ok(Json(RebindGhInstallResp {
            installation_id: install_row_id,
            tenant_id,
            previous_tenant_id: None,
        })
        .into_response());
    }

    // Different tenant — require force.
    if !body.force {
        write_audit_row(
            &state.pool,
            &actor,
            "tenant.rebind_gh",
            Some(tenant_id),
            None,
            request_id,
            None,
            None,
            &serde_json::json!({
                "github_installation_id": body.github_installation_id,
                "current_tenant_id": current_tenant_id,
            }),
            "denied",
            Some("cross_tenant_conflict"),
        )
        .await;
        return Err(AppError::GithubInstallationConflict);
    }

    sqlx::query("UPDATE control.github_installations SET tenant_id = $1 WHERE id = $2")
        .bind(tenant_id)
        .bind(install_row_id)
        .execute(&state.pool)
        .await?;

    write_audit_row(
        &state.pool,
        &actor,
        "tenant.rebind_gh",
        Some(tenant_id),
        None,
        request_id,
        None,
        None,
        &serde_json::json!({
            "github_installation_id": body.github_installation_id,
            "previous_tenant_id": current_tenant_id,
            "reason": body.reason,
        }),
        "ok",
        None,
    )
    .await;

    tracing::warn!(
        actor = %actor,
        %tenant_id,
        %current_tenant_id,
        github_installation_id = body.github_installation_id,
        reason = ?body.reason,
        "cross-tenant GitHub installation rebind executed"
    );

    Ok(Json(RebindGhInstallResp {
        installation_id: install_row_id,
        tenant_id,
        previous_tenant_id: Some(current_tenant_id),
    })
    .into_response())
}

// ---------------------------------------------------------------------------
// ─── impersonate ───────────────────────────────────────────────────────────
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ImpersonateReq {
    /// User to impersonate (must be a member of `tenant_id`).
    pub user_id: Uuid,
    /// Requested session duration in seconds. Server-enforced ceiling: 900 s.
    #[serde(default = "default_impersonation_secs")]
    pub duration_secs: i64,
}

fn default_impersonation_secs() -> i64 {
    IMPERSONATION_MAX_SECS
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

// ---------------------------------------------------------------------------
// ─── force-delete ──────────────────────────────────────────────────────────
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ForceDeleteReq {
    /// When present, this is phase-2: execute the deletion after verifying the token.
    pub confirm_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ForceDeleteDryRunResp {
    pub confirm_token: String,
    pub snapshot: ForceDeleteSnapshot,
    pub expires_in_secs: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ForceDeleteSnapshot {
    pub tenant_id: Uuid,
    pub repos: i64,
    pub sessions: i64,
    pub api_keys: i64,
    pub members: i64,
    pub ingestion_runs: i64,
    pub github_installations: i64,
}

#[derive(Debug, Serialize)]
pub struct ForceDeleteResp {
    pub tenant_id: Uuid,
    pub status: String,
}

/// Claims for the phase-1 confirm token.
#[derive(Debug, Serialize, Deserialize)]
struct ConfirmTokenClaims {
    tenant_id: String,
    actor: String,
    snapshot_hash: String,
    exp: usize,
    typ: String,
}

pub async fn force_delete(
    State(state): State<AppState>,
    Extension(AdminActor(actor)): Extension<AdminActor>,
    Extension(AdminRequestId(request_id)): Extension<AdminRequestId>,
    Path(tenant_id): Path<Uuid>,
    Json(body): Json<ForceDeleteReq>,
) -> Result<Response, AppError> {
    match body.confirm_token {
        None => force_delete_phase1(&state, actor, tenant_id, request_id).await,
        Some(token) => force_delete_phase2(&state, actor, tenant_id, request_id, &token).await,
    }
}

async fn force_delete_phase1(
    state: &AppState,
    actor: String,
    tenant_id: Uuid,
    request_id: Uuid,
) -> Result<Response, AppError> {
    let snapshot = fetch_snapshot(state, tenant_id).await?;
    let snapshot_json = serde_json::to_string(&snapshot).unwrap_or_default();
    let snapshot_hash = hex::encode(sha2::Sha256::digest(snapshot_json.as_bytes()));

    let admin_token = state.config.admin_token.as_deref().unwrap_or("");
    let exp = Utc::now()
        .timestamp()
        .saturating_add(FORCE_DELETE_CONFIRM_TTL_SECS);

    let claims = ConfirmTokenClaims {
        tenant_id: tenant_id.to_string(),
        actor: actor.clone(),
        snapshot_hash,
        exp: usize::try_from(exp).unwrap_or(0),
        typ: "force_delete_confirm".to_owned(),
    };

    let key_bytes = sha2::Sha256::digest(format!("{admin_token}:fd_confirm").as_bytes());
    let encoding_key = EncodingKey::from_secret(&key_bytes);
    let confirm_token =
        jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &encoding_key)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("confirm token encode: {e}")))?;

    write_audit_row(
        &state.pool,
        &actor,
        "tenant.force_delete.dry_run",
        Some(tenant_id),
        None,
        request_id,
        None,
        None,
        &serde_json::json!({
            "snapshot": &snapshot,
        }),
        "ok",
        None,
    )
    .await;

    Ok(Json(ForceDeleteDryRunResp {
        confirm_token,
        snapshot,
        expires_in_secs: u64::try_from(FORCE_DELETE_CONFIRM_TTL_SECS).unwrap_or(60),
    })
    .into_response())
}

#[allow(clippy::too_many_lines)]
async fn force_delete_phase2(
    state: &AppState,
    actor: String,
    tenant_id: Uuid,
    request_id: Uuid,
    confirm_token: &str,
) -> Result<Response, AppError> {
    let admin_token = state.config.admin_token.as_deref().unwrap_or("");
    let key_bytes = sha2::Sha256::digest(format!("{admin_token}:fd_confirm").as_bytes());
    let decoding_key = jsonwebtoken::DecodingKey::from_secret(&key_bytes);
    let mut validation = jsonwebtoken::Validation::new(Algorithm::HS256);
    validation.validate_exp = true;

    let token_data =
        jsonwebtoken::decode::<ConfirmTokenClaims>(confirm_token, &decoding_key, &validation)
            .map_err(|_| AppError::InvalidToken)?;

    let claims = token_data.claims;

    // Verify actor + tenant binding.
    if claims.actor != actor {
        write_audit_row(
            &state.pool,
            &actor,
            "tenant.force_delete.execute",
            Some(tenant_id),
            None,
            request_id,
            None,
            None,
            &serde_json::json!({"error": "actor_mismatch"}),
            "denied",
            Some("actor_mismatch"),
        )
        .await;
        return Err(AppError::InvalidToken);
    }
    if claims.tenant_id != tenant_id.to_string() {
        write_audit_row(
            &state.pool,
            &actor,
            "tenant.force_delete.execute",
            Some(tenant_id),
            None,
            request_id,
            None,
            None,
            &serde_json::json!({"error": "tenant_id_mismatch"}),
            "denied",
            Some("tenant_id_mismatch"),
        )
        .await;
        return Err(AppError::InvalidToken);
    }

    // Re-compute snapshot and verify hash hasn't shifted.
    let current_snapshot = fetch_snapshot(state, tenant_id).await?;
    let current_json = serde_json::to_string(&current_snapshot).unwrap_or_default();
    let current_hash = hex::encode(sha2::Sha256::digest(current_json.as_bytes()));
    if current_hash != claims.snapshot_hash {
        write_audit_row(
            &state.pool,
            &actor,
            "tenant.force_delete.execute",
            Some(tenant_id),
            None,
            request_id,
            None,
            None,
            &serde_json::json!({"error": "snapshot_shifted"}),
            "denied",
            Some("snapshot_shifted"),
        )
        .await;
        // The token is technically valid but the tenant's data changed since phase-1.
        return Err(AppError::AdminForceDeleteConflict);
    }

    // Execute: soft-delete + Kafka tombstone (mirrors existing delete_tenant logic).
    let mut txn = state.pool.begin().await?;

    sqlx::query(
        "UPDATE control.tenants \
         SET status = 'deleting', deleted_at = now() \
         WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(tenant_id)
    .execute(&mut *txn)
    .await?;

    sqlx::query(
        "UPDATE control.ingestion_runs \
         SET status = 'cancelled' \
         WHERE tenant_id = $1 AND status IN ('queued', 'running')",
    )
    .bind(tenant_id)
    .execute(&mut *txn)
    .await?;

    if let Some(producer) = state.tombstone_producer.as_ref() {
        use rb_kafka::EventEnvelope;
        use rb_schemas::{TenantId, Tombstone};

        let tombstone = Tombstone {
            tenant_id: tenant_id.to_string(),
            repo_id: String::new(),
            requested_by: actor.clone(),
            emitted_at_ms: Utc::now().timestamp_millis(),
        };
        let envelope = EventEnvelope::new(TenantId::from(tenant_id), tombstone);
        let key = tenant_id.to_string();

        if let Err(e) = producer
            .publish("rb.tombstones.v1", key.as_bytes(), envelope)
            .await
        {
            txn.rollback().await.ok();
            write_audit_row(
                &state.pool,
                &actor,
                "tenant.force_delete.execute",
                Some(tenant_id),
                None,
                request_id,
                None,
                None,
                &serde_json::json!({"error": "kafka_publish_failed"}),
                "error",
                Some("kafka_publish_failed"),
            )
            .await;
            return Err(AppError::KafkaPublish(e));
        }
    }

    txn.commit().await?;

    write_audit_row(
        &state.pool,
        &actor,
        "tenant.force_delete.execute",
        Some(tenant_id),
        None,
        request_id,
        None,
        None,
        &serde_json::json!({"snapshot": current_snapshot}),
        "ok",
        None,
    )
    .await;

    tracing::warn!(
        actor = %actor,
        %tenant_id,
        "admin force-delete executed; tombstone emitted"
    );

    Ok(Json(ForceDeleteResp {
        tenant_id,
        status: "deleting".to_owned(),
    })
    .into_response())
}

async fn fetch_snapshot(
    state: &AppState,
    tenant_id: Uuid,
) -> Result<ForceDeleteSnapshot, AppError> {
    let (repos,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM control.repos WHERE tenant_id = $1 AND archived_at IS NULL",
    )
    .bind(tenant_id)
    .fetch_one(&state.pool)
    .await?;

    let (sessions,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM control.sessions WHERE tenant_id = $1 AND revoked_at IS NULL",
    )
    .bind(tenant_id)
    .fetch_one(&state.pool)
    .await?;

    let (api_keys,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM control.api_keys WHERE tenant_id = $1 AND revoked_at IS NULL",
    )
    .bind(tenant_id)
    .fetch_one(&state.pool)
    .await?;

    let (members,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM control.tenant_members WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_one(&state.pool)
            .await?;

    let (ingestion_runs,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM control.ingestion_runs WHERE tenant_id = $1 AND status IN ('queued', 'running')",
    )
    .bind(tenant_id)
    .fetch_one(&state.pool)
    .await?;

    let (github_installations,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM control.github_installations WHERE tenant_id = $1 AND deleted_at IS NULL",
    )
    .bind(tenant_id)
    .fetch_one(&state.pool)
    .await?;

    Ok(ForceDeleteSnapshot {
        tenant_id,
        repos,
        sessions,
        api_keys,
        members,
        ingestion_runs,
        github_installations,
    })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impersonation_max_secs_is_900() {
        assert_eq!(IMPERSONATION_MAX_SECS, 900);
    }

    #[test]
    fn force_delete_confirm_ttl_is_60() {
        assert_eq!(FORCE_DELETE_CONFIRM_TTL_SECS, 60);
    }

    #[test]
    fn dry_run_resp_serializes() {
        let snap = ForceDeleteSnapshot {
            tenant_id: Uuid::new_v4(),
            repos: 2,
            sessions: 1,
            api_keys: 3,
            members: 4,
            ingestion_runs: 0,
            github_installations: 1,
        };
        let v = serde_json::to_value(&snap).unwrap();
        assert_eq!(v["repos"], 2);
        assert_eq!(v["members"], 4);
    }

    #[test]
    fn duration_clamped_to_ceiling() {
        let clamped = 9999_i64.min(IMPERSONATION_MAX_SECS);
        assert_eq!(clamped, 900);
    }
}
