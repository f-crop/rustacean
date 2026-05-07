use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::IntoResponse,
};
use rb_sse::EventId;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{require_session, AuthContext},
    state::AppState,
};

pub async fn session_events(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    let session = require_session(&auth)?;
    let tenant_id = session.tenant_id;

    let row = sqlx::query(
        r#"
        SELECT 1 FROM agent_sessions WHERE id = $1 AND tenant_id = $2
        "#,
    )
    .bind(session_id)
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Database)?;

    let _ = row.ok_or(AppError::NotFound)?;

    let last_event_id = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(|s| EventId::from(s.to_owned()));

    let tenant_id_obj = rb_schemas::TenantId::from(tenant_id);

    Ok(state
        .sse_bus
        .subscribe(&tenant_id_obj, last_event_id.as_ref()))
}
