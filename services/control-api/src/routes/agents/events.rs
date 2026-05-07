//! `GET /v1/agents/sessions/{id}/events` — SSE live event stream (ADR-009 §5).

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::IntoResponse,
};
use rb_schemas::TenantId;
use rb_sse::EventId;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, require_verified_session},
    state::AppState,
};

#[utoipa::path(
    get,
    path = "/v1/agents/sessions/{id}/events",
    params(("id" = Uuid, Path, description = "Session ID")),
    responses(
        (status = 200, description = "SSE stream"),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Session belongs to different tenant"),
        (status = 404, description = "Session not found"),
    ),
    tag = "agents"
)]
pub async fn session_events(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    auth: AuthContext,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    let session = require_verified_session(auth)?;

    // Dynamic query — agents schema not in sqlx offline cache yet.
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT tenant_id FROM agents.agent_sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("DB error in session_events: {e}");
        AppError::Internal(anyhow::anyhow!("DB query failed"))
    })?;

    let (session_tenant_id,) = row.ok_or(AppError::NotFound)?;

    if session_tenant_id != session.tenant_id {
        return Err(AppError::InsufficientRole);
    }

    let tenant_id = TenantId::from(session.tenant_id);

    let last_event_id = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(|s| EventId::from(s.to_owned()));

    Ok(state.sse_bus.subscribe(&tenant_id, last_event_id.as_ref()))
}
