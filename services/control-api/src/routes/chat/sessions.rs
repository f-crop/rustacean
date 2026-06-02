//! Chat session lifecycle routes (ADR-013 §3).
//!
//! - `POST /v1/chat/sessions`      — create session, mint MCP JWT, dispatch to agent-runner
//! - `GET  /v1/chat/sessions/{id}` — fetch session with message history (tenant-scoped)

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use rb_auth::{McpTokenClaims, mint_mcp_token};
use rb_kafka::EventEnvelope;
use rb_schemas::{AgentSessionCommand, AgentSessionStart, TenantId};
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
    db_list_chat_messages,
};

const TOPIC_AGENT_COMMANDS: &str = "rb.agent.commands";
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
        (status = 503, description = "Kafka unavailable"),
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

    let jwt_secret = &state.mcp_jwt_secret;

    let session_id = Uuid::new_v4();
    let trace_id = format!("{}", Uuid::new_v4().as_simple());

    // Insert session row before dispatching to agent-runner.
    db_insert_chat_session(
        &state.pool,
        session_id,
        caller.tenant_id,
        caller.user_id,
        &req.runtime,
        &trace_id,
    )
    .await?;

    // Mint a short-lived read-scoped MCP JWT for this session.
    let token = mint_mcp_token(
        jwt_secret.as_bytes(),
        state.config.mcp_jwt_ttl_secs,
        McpTokenClaims {
            sub: session_id,
            tenant_id: caller.tenant_id,
            user_id: caller.user_id,
        },
    )
    .map_err(|e| AppError::Internal(anyhow::anyhow!("JWT mint failed: {e}")))?;

    // Dispatch to agent-runner via existing rb.agent.commands Kafka topic.
    // The JWT is passed as api_key; agent-runner writes it to .mcp.json (ADR-013 §5.4).
    let workspace_path = format!("{}/{}", caller.tenant_id, session_id);
    let runtime_val: i32 = parse_runtime(&req.runtime)
        .ok_or(AppError::InvalidInput)?
        .into();
    let command = AgentSessionCommand {
        session_id: session_id.to_string(),
        command: Some(rb_schemas::agent_session_command::Command::Start(
            AgentSessionStart {
                runtime: runtime_val,
                initial_prompt: String::new(),
                workspace_path,
                api_key: token,
            },
        )),
    };

    let producer = state
        .agent_commands_producer
        .as_ref()
        .ok_or(AppError::KafkaNotConfigured)?;

    let tenant_id = TenantId::from(caller.tenant_id);
    let envelope = EventEnvelope::new(tenant_id, command);

    producer
        .publish(TOPIC_AGENT_COMMANDS, session_id.as_bytes(), envelope)
        .await
        .map_err(|e| {
            tracing::error!("failed to publish chat session start: {e}");
            AppError::Internal(anyhow::anyhow!("Kafka publish failed"))
        })?;

    Ok((
        StatusCode::ACCEPTED,
        Json(CreateChatSessionResponse {
            session_id,
            status: "active".to_owned(),
        }),
    ))
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
