//! Chat session lifecycle routes (ADR-013 §3).
//!
//! - `POST /v1/chat/sessions`      — create session row; agent is dispatched on first message
//! - `GET  /v1/chat/sessions/{id}` — fetch session with message history (tenant-scoped)

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, Scope},
    routes::agents::session_lifecycle::parse_runtime,
    state::AppState,
};

use super::db::{
    ChatMessageRow, ChatSessionRow, db_get_chat_session, db_insert_chat_session,
    db_list_chat_messages, db_list_chat_sessions,
};
const MAX_RUNTIME_LEN: usize = 32;

// ---------------------------------------------------------------------------
// Request / response DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateChatSessionRequest {
    /// Runtime to use: `"claude_code"`, `"opencode"`, or `"pi"`.
    pub runtime: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct CreateChatSessionResponse {
    pub session_id: Uuid,
    pub status: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ChatMessageDto {
    pub id: Uuid,
    pub seq: i32,
    pub role: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

impl From<ChatMessageRow> for ChatMessageDto {
    fn from(r: ChatMessageRow) -> Self {
        Self {
            id: r.id,
            seq: r.seq,
            role: r.role,
            body: r.body,
            created_at: r.created_at,
        }
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ChatSessionDto {
    pub id: Uuid,
    pub runtime: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub last_activity_at: DateTime<Utc>,
    pub messages: Vec<ChatMessageDto>,
}

impl ChatSessionDto {
    fn from_row(row: ChatSessionRow, messages: Vec<ChatMessageRow>) -> Self {
        Self {
            id: row.id,
            runtime: row.runtime,
            status: row.status,
            created_at: row.created_at,
            last_activity_at: row.last_activity_at,
            messages: messages.into_iter().map(ChatMessageDto::from).collect(),
        }
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ChatSessionSummary {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Option<Uuid>,
    pub runtime: String,
    pub status: String,
    pub trace_id: String,
    pub created_at: DateTime<Utc>,
    pub last_activity_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

impl From<ChatSessionRow> for ChatSessionSummary {
    fn from(r: ChatSessionRow) -> Self {
        Self {
            id: r.id,
            tenant_id: r.tenant_id,
            user_id: r.user_id,
            runtime: r.runtime,
            status: r.status,
            trace_id: r.trace_id,
            created_at: r.created_at,
            last_activity_at: r.last_activity_at,
            ended_at: r.ended_at,
        }
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ListChatSessionsResponse {
    pub sessions: Vec<ChatSessionSummary>,
}

// ---------------------------------------------------------------------------
// POST /v1/chat/sessions
// ---------------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/v1/chat/sessions",
    request_body = CreateChatSessionRequest,
    responses(
        (status = 202, description = "Chat session created"),
        (status = 400, description = "Invalid runtime"),
        (status = 401, description = "Authentication required"),
        (status = 404, description = "Feature not enabled"),
    ),
    tag = "chat"
)]
pub async fn create_chat_session(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(req): Json<CreateChatSessionRequest>,
) -> Result<impl IntoResponse, AppError> {
    if !state.config.chat_panel_enabled {
        return Err(AppError::ChatFeatureDisabled);
    }

    let caller = require_chat_auth(auth)?;

    if req.runtime.len() > MAX_RUNTIME_LEN || parse_runtime(&req.runtime).is_none() {
        return Err(AppError::InvalidInput);
    }

    let session_id = Uuid::new_v4();
    let trace_id = format!("{}", Uuid::new_v4().as_simple());

    // Insert session row.  Agent dispatch happens on the first user message
    // (POST /v1/chat/sessions/{id}/messages) so that claude receives the
    // initial_prompt immediately — avoiding the 3-second stdin-timeout
    // built into `claude -p` when no input arrives within that window.
    db_insert_chat_session(
        &state.pool,
        session_id,
        caller.tenant_id,
        caller.user_id,
        &req.runtime,
        &trace_id,
    )
    .await?;

    Ok((
        StatusCode::ACCEPTED,
        Json(CreateChatSessionResponse {
            session_id,
            status: "active".to_owned(),
        }),
    ))
}

// ---------------------------------------------------------------------------
// GET /v1/chat/sessions
// ---------------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/v1/chat/sessions",
    responses(
        (status = 200, description = "List of chat sessions for the authenticated user", body = ListChatSessionsResponse),
        (status = 401, description = "Authentication required"),
        (status = 404, description = "Feature not enabled"),
    ),
    tag = "chat"
)]
pub async fn list_chat_sessions(
    State(state): State<AppState>,
    auth: AuthContext,
) -> Result<impl IntoResponse, AppError> {
    if !state.config.chat_panel_enabled {
        return Err(AppError::ChatFeatureDisabled);
    }

    let caller = require_chat_auth(auth)?;

    let rows = db_list_chat_sessions(&state.pool, caller.tenant_id, caller.user_id, 50).await?;
    let sessions: Vec<ChatSessionSummary> =
        rows.into_iter().map(ChatSessionSummary::from).collect();

    Ok(Json(ListChatSessionsResponse { sessions }))
}

// ---------------------------------------------------------------------------
// GET /v1/chat/sessions/{id}
// ---------------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/v1/chat/sessions/{id}",
    params(("id" = Uuid, Path, description = "Chat session ID")),
    responses(
        (status = 200, description = "Session with message history"),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Session not found or feature disabled"),
    ),
    tag = "chat"
)]
pub async fn get_chat_session(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    auth: AuthContext,
) -> Result<impl IntoResponse, AppError> {
    if !state.config.chat_panel_enabled {
        return Err(AppError::ChatFeatureDisabled);
    }

    let caller = require_chat_auth(auth)?;

    let session = db_get_chat_session(&state.pool, session_id, caller.tenant_id).await?;

    let messages =
        db_list_chat_messages(&state.pool, session_id, caller.tenant_id, 100, None).await?;

    Ok(Json(ChatSessionDto::from_row(session, messages)))
}

// ---------------------------------------------------------------------------
// Auth helper
// ---------------------------------------------------------------------------

/// Tenant and user extracted from a verified chat request.
#[derive(Debug, Clone, Copy)]
pub struct ChatCaller {
    pub tenant_id: Uuid,
    pub user_id: Uuid,
}

/// Require a verified browser session or any API key with a valid scope.
pub fn require_chat_auth(auth: AuthContext) -> Result<ChatCaller, AppError> {
    match auth {
        AuthContext::Session(info) if info.email_verified => Ok(ChatCaller {
            tenant_id: info.tenant_id,
            user_id: info.user_id,
        }),
        AuthContext::Session(_) => Err(AppError::EmailNotVerified),
        AuthContext::ExpiredSession => Err(AppError::SessionExpired),
        AuthContext::ApiKey(info)
            if info
                .scopes
                .iter()
                .any(|s| matches!(s, Scope::Read | Scope::Write | Scope::Agent | Scope::Admin)) =>
        {
            Ok(ChatCaller {
                tenant_id: info.tenant_id,
                user_id: info.user_id,
            })
        }
        AuthContext::ApiKey(_) => Err(AppError::InsufficientScope),
        AuthContext::McpJwt(_) | AuthContext::Anonymous => Err(AppError::Unauthorized),
    }
}
