use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use rb_kafka::EventEnvelope;
use rb_schemas::{TenantId, Tombstone};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use super::role::{TenantRole, require_role, require_session};
use crate::{error::AppError, middleware::auth::AuthContext, state::AppState};

pub(super) const TOMBSTONES_TOPIC: &str = "rb.tombstones.v1";

#[derive(Debug, Serialize, ToSchema)]
pub struct DeleteTenantResponse {
    /// Tenant ID queued for deletion.
    pub tenant_id: Uuid,
    /// Status after this call: `"deleting"`.
    pub status: String,
}

/// Delete a tenant (owner-only, requires typed confirmation).
///
/// Soft-deletes the tenant by setting `deleted_at` and transitioning its
/// status to `deleting`, cancels all in-flight ingestion runs, then emits a
/// `Tombstone` to `rb.tombstones.v1`. The tombstoner service performs the
/// async data-plane cleanup (`PostgreSQL` schema drop, `Neo4j` node removal,
/// `Qdrant` point deletion).
///
/// **Idempotent**: repeating the call on a tenant already in `deleting` or
/// `deleted` state returns `204 No Content`.
///
/// **Typed confirmation**: the `X-Confirm` header must match the tenant slug
/// exactly (case-insensitive) to prevent accidental deletions.
///
/// Returns `503` if the Kafka producer is not available — the request can be
/// retried once the broker is reachable.
#[utoipa::path(
    delete,
    path = "/v1/tenants/{id}",
    params(
        ("id" = Uuid, Path, description = "Tenant ID"),
        ("X-Confirm" = String, Header, description = "Must equal the tenant slug"),
    ),
    responses(
        (status = 202, description = "Deletion queued — tombstone emitted", body = DeleteTenantResponse),
        (status = 204, description = "Already deleted (idempotent)"),
        (status = 400, description = "X-Confirm header missing or slug mismatch (confirmation_mismatch)"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Insufficient role — must be owner (insufficient_role)"),
        (status = 404, description = "Tenant not found"),
        (status = 503, description = "Kafka producer not available (kafka_not_configured, kafka_unavailable)"),
    ),
    tag = "tenants"
)]
pub async fn delete_tenant(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(tenant_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let session = require_session(auth)?;
    require_role(&state.pool, session.user_id, tenant_id, TenantRole::Owner).await?;

    let confirm = headers
        .get("x-confirm")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim();
    if confirm.is_empty() {
        return Err(AppError::ConfirmationMismatch);
    }

    let producer = state
        .tombstone_producer
        .as_ref()
        .ok_or(AppError::KafkaNotConfigured)?;

    let row: Option<(String, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT slug::text, deleted_at \
         FROM control.tenants \
         WHERE id = $1",
    )
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await?;

    let (slug, deleted_at) = row.ok_or(AppError::NotFound)?;

    // Idempotent: already soft-deleted.
    if deleted_at.is_some() {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }

    // Case-insensitive slug comparison (slug is CITEXT in Postgres).
    if !slug.eq_ignore_ascii_case(confirm) {
        return Err(AppError::ConfirmationMismatch);
    }

    // Probe broker reachability before touching the DB.
    if !producer
        .check_ready(std::time::Duration::from_millis(500))
        .await
    {
        return Err(AppError::KafkaUnavailable);
    }

    let tombstone = Tombstone {
        tenant_id: tenant_id.to_string(),
        repo_id: String::new(),
        requested_by: session.user_id.to_string(),
        emitted_at_ms: chrono::Utc::now().timestamp_millis(),
    };
    let envelope = EventEnvelope::new(TenantId::from(tenant_id), tombstone);
    let partition_key = tenant_id.to_string();

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

    if let Err(e) = producer
        .publish(TOMBSTONES_TOPIC, partition_key.as_bytes(), envelope)
        .await
    {
        txn.rollback().await.ok();
        return Err(AppError::KafkaPublish(e));
    }

    txn.commit().await?;

    tracing::info!(
        %tenant_id,
        user_id = %session.user_id,
        "tenant deletion queued; tombstone emitted to rb.tombstones.v1"
    );

    Ok((
        StatusCode::ACCEPTED,
        Json(DeleteTenantResponse {
            tenant_id,
            status: "deleting".to_owned(),
        }),
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{DeleteTenantResponse, TOMBSTONES_TOPIC};
    use crate::error::AppError;
    use axum::response::IntoResponse;
    use rb_schemas::Tombstone;
    use uuid::Uuid;

    #[test]
    fn error_confirmation_mismatch_message() {
        assert_eq!(
            AppError::ConfirmationMismatch.to_string(),
            "X-Confirm header must match the tenant slug exactly"
        );
    }

    #[test]
    fn error_confirmation_mismatch_is_bad_request() {
        let resp = AppError::ConfirmationMismatch.into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn delete_tenant_response_serializes() {
        let id = Uuid::new_v4();
        let resp = DeleteTenantResponse {
            tenant_id: id,
            status: "deleting".to_owned(),
        };
        let val = serde_json::to_value(&resp).unwrap();
        assert_eq!(val["status"], "deleting");
        assert_eq!(val["tenant_id"], id.to_string());
    }

    #[test]
    fn tombstone_topic_constant() {
        assert_eq!(TOMBSTONES_TOPIC, "rb.tombstones.v1");
    }

    #[test]
    fn tombstone_tenant_wide_has_empty_repo_id() {
        let tombstone = Tombstone {
            tenant_id: Uuid::new_v4().to_string(),
            repo_id: String::new(),
            requested_by: Uuid::new_v4().to_string(),
            emitted_at_ms: 0,
        };
        assert!(
            tombstone.repo_id.is_empty(),
            "tenant-wide tombstone must have empty repo_id"
        );
    }

    #[test]
    fn slug_comparison_is_case_insensitive() {
        let slug = "my-tenant";
        assert!(slug.eq_ignore_ascii_case("MY-TENANT"));
        assert!(slug.eq_ignore_ascii_case("My-Tenant"));
        assert!(!slug.eq_ignore_ascii_case("other-tenant"));
    }
}
