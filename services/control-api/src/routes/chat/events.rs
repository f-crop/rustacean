//! `GET /v1/chat/sessions/{id}/events` — SSE live event stream for chat sessions.
//!
//! Reuses the existing `rb-sse::EventBus::subscribe_session` mechanism.
//! Events are published by agent-runner via the internal event ingest endpoint.

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::{
        IntoResponse,
        sse::{KeepAlive, Sse},
    },
};
use rb_schemas::TenantId;
use rb_sse::EventId;
use uuid::Uuid;

use crate::{error::AppError, middleware::auth::AuthContext, state::AppState};

use super::db::db_get_chat_session;
use super::sessions::require_chat_auth;

#[utoipa::path(
    get,
    path = "/v1/chat/sessions/{id}/events",
    params(("id" = Uuid, Path, description = "Chat session ID")),
    responses(
        (status = 200, description = "SSE stream of chat events"),
        (status = 401, description = "Authentication required"),
        (status = 404, description = "Session not found or feature disabled"),
    ),
    tag = "chat"
)]
pub async fn chat_session_events(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    auth: AuthContext,
    headers: HeaderMap,
) -> Result<axum::response::Response, AppError> {
    if !state.config.chat_panel_enabled {
        return Err(AppError::ChatFeatureDisabled);
    }

    let caller = require_chat_auth(auth)?;

    // Verify session ownership before opening the stream.
    db_get_chat_session(&state.pool, session_id, caller.tenant_id).await?;

    let last_event_id = headers
        .get("Last-Event-ID")
        .and_then(|v| v.to_str().ok())
        .map(EventId::from);

    let tenant_id = TenantId::from(caller.tenant_id);
    let stream = state
        .sse_bus
        .subscribe_session(&tenant_id, &session_id, last_event_id.as_ref());

    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}
