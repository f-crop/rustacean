use axum::{
    Extension, Json,
    extract::{Path, State},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::admin_auth::{AdminActor, AdminIp, AdminRequestId, AdminUserAgent},
    routes::admin::v1::write_audit_row,
    state::AppState,
};

/// Confirm-token TTL for the two-phase force-delete (60 seconds).
pub(super) const FORCE_DELETE_CONFIRM_TTL_SECS: i64 = 60;

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
    Extension(AdminIp(ip)): Extension<AdminIp>,
    Extension(AdminUserAgent(user_agent)): Extension<AdminUserAgent>,
    Path(tenant_id): Path<Uuid>,
    Json(body): Json<ForceDeleteReq>,
) -> Result<Response, AppError> {
    match body.confirm_token {
        None => force_delete_phase1(&state, actor, tenant_id, request_id, ip, user_agent).await,
        Some(token) => {
            force_delete_phase2(&state, actor, tenant_id, request_id, ip, user_agent, &token).await
        }
    }
}

async fn force_delete_phase1(
    state: &AppState,
    actor: String,
    tenant_id: Uuid,
    request_id: Uuid,
    ip: Option<String>,
    user_agent: Option<String>,
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
        ip.as_deref(),
        user_agent.as_deref(),
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
    ip: Option<String>,
    user_agent: Option<String>,
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
            ip.as_deref(),
            user_agent.as_deref(),
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
            ip.as_deref(),
            user_agent.as_deref(),
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
            ip.as_deref(),
            user_agent.as_deref(),
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
                ip.as_deref(),
                user_agent.as_deref(),
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
        ip.as_deref(),
        user_agent.as_deref(),
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
