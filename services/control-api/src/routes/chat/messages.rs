//! Chat message routes (ADR-013 §3).
//!
//! - `POST /v1/chat/sessions/{id}/messages` — append user message, dispatch stdin turn
//! - `GET  /v1/chat/sessions/{id}/messages` — paginated message history

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use rb_auth::{McpTokenClaims, mint_mcp_token};
use rb_kafka::EventEnvelope;
use rb_schemas::{
    AgentSessionCommand, AgentSessionInput, AgentSessionStart, TenantId, agent_session_command,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::AppError, middleware::auth::AuthContext,
    routes::agents::session_lifecycle::parse_runtime, state::AppState,
};

use super::db::{db_get_chat_session, db_insert_chat_message, db_list_chat_messages};
use super::sessions::{ChatMessageDto, require_chat_auth};

const TOPIC_AGENT_COMMANDS: &str = "rb.agent.commands";
const MESSAGE_BODY_MAX_BYTES: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// POST /v1/chat/sessions/{id}/messages
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct PostMessageRequest {
    pub content: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct PostMessageResponse {
    pub message_id: Uuid,
    pub seq: i32,
}

#[utoipa::path(
    post,
    path = "/v1/chat/sessions/{id}/messages",
    params(("id" = Uuid, Path, description = "Chat session ID")),
    request_body = PostMessageRequest,
    responses(
        (status = 202, description = "Message appended and dispatched"),
        (status = 400, description = "Message body too large"),
        (status = 401, description = "Authentication required"),
        (status = 404, description = "Session not found or feature disabled"),
        (status = 422, description = "Session is not active"),
        (status = 503, description = "Kafka unavailable"),
    ),
    tag = "chat"
)]
#[allow(clippy::too_many_lines)]
pub async fn post_chat_message(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    auth: AuthContext,
    Json(req): Json<PostMessageRequest>,
) -> Result<impl IntoResponse, AppError> {
    if !state.config.chat_panel_enabled {
        return Err(AppError::ChatFeatureDisabled);
    }

    let caller = require_chat_auth(auth)?;

    if req.content.len() > MESSAGE_BODY_MAX_BYTES {
        return Err(AppError::InvalidInput);
    }

    let session = db_get_chat_session(&state.pool, session_id, caller.tenant_id).await?;

    if session.status != "active" {
        return Err(AppError::ChatSessionNotActive);
    }

    let message_id = Uuid::new_v4();
    let seq = db_insert_chat_message(
        &state.pool,
        message_id,
        session_id,
        caller.tenant_id,
        "user",
        &req.content,
    )
    .await?;

    let producer = state
        .agent_commands_producer
        .as_ref()
        .ok_or(AppError::KafkaNotConfigured)?;

    let tenant_id = TenantId::from(caller.tenant_id);

    if seq == 1 {
        // First message: publish Start (empty initial_prompt → agent spawns in
        // stream-json mode and stays alive for multi-turn), then immediately
        // publish the user content as an Input turn.  Both messages share the
        // same Kafka partition key (session_id) so they are consumed in order.
        let workspace_path = format!("{}/{}", caller.tenant_id, session_id);
        let runtime_val: i32 = parse_runtime(&session.runtime)
            .ok_or(AppError::InvalidInput)?
            .into();

        // Guard: RB_MCP_JWT_SECRET must be set. Without it the minted JWT is
        // signed with an empty key, and auth.rs skips JWT verification
        // (requires non-empty secret), causing the MCP server to be treated as
        // Anonymous on every /mcp call — tools silently unavailable in chat.
        if state.mcp_jwt_secret.is_empty() {
            tracing::error!(
                "RB_MCP_JWT_SECRET not configured; cannot mint MCP token for chat session"
            );
            return Err(AppError::Internal(anyhow::anyhow!(
                "MCP JWT secret not configured (RB_MCP_JWT_SECRET)"
            )));
        }

        let token = mint_mcp_token(
            state.mcp_jwt_secret.as_bytes(),
            state.config.mcp_jwt_ttl_secs,
            McpTokenClaims {
                sub: session_id,
                tenant_id: caller.tenant_id,
                user_id: caller.user_id,
            },
        )
        .map_err(|e| AppError::Internal(anyhow::anyhow!("JWT mint failed: {e}")))?;

        let start_cmd = AgentSessionCommand {
            session_id: session_id.to_string(),
            command: Some(agent_session_command::Command::Start(AgentSessionStart {
                runtime: runtime_val,
                initial_prompt: String::new(),
                workspace_path,
                api_key: token,
            })),
        };
        producer
            .publish(
                TOPIC_AGENT_COMMANDS,
                session_id.as_bytes(),
                EventEnvelope::new(tenant_id, start_cmd),
            )
            .await
            .map_err(|e| {
                tracing::error!("failed to publish chat start: {e}");
                AppError::Internal(anyhow::anyhow!("Kafka publish failed"))
            })?;

        let input_cmd = AgentSessionCommand {
            session_id: session_id.to_string(),
            command: Some(agent_session_command::Command::Input(AgentSessionInput {
                input: req.content,
            })),
        };
        producer
            .publish(
                TOPIC_AGENT_COMMANDS,
                session_id.as_bytes(),
                EventEnvelope::new(tenant_id, input_cmd),
            )
            .await
            .map_err(|e| {
                tracing::error!("failed to publish chat input (seq=1): {e}");
                AppError::Internal(anyhow::anyhow!("Kafka publish failed"))
            })?;
    } else {
        // Subsequent messages: forward as stdin Input turns directly.
        let input_cmd = AgentSessionCommand {
            session_id: session_id.to_string(),
            command: Some(agent_session_command::Command::Input(AgentSessionInput {
                input: req.content,
            })),
        };
        producer
            .publish(
                TOPIC_AGENT_COMMANDS,
                session_id.as_bytes(),
                EventEnvelope::new(tenant_id, input_cmd),
            )
            .await
            .map_err(|e| {
                tracing::error!("failed to publish chat input: {e}");
                AppError::Internal(anyhow::anyhow!("Kafka publish failed"))
            })?;
    }

    Ok((
        StatusCode::ACCEPTED,
        Json(PostMessageResponse { message_id, seq }),
    ))
}

// ---------------------------------------------------------------------------
// GET /v1/chat/sessions/{id}/messages
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct ListMessagesParams {
    /// Return messages with seq > `after_seq` (for pagination).
    pub after_seq: Option<i32>,
    /// Max messages to return (default 50, max 200).
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ListMessagesResponse {
    pub messages: Vec<ChatMessageDto>,
    pub has_more: bool,
}

#[utoipa::path(
    get,
    path = "/v1/chat/sessions/{id}/messages",
    params(
        ("id" = Uuid, Path, description = "Chat session ID"),
        ("after_seq" = Option<i32>, Query, description = "Cursor: return messages after this seq"),
        ("limit" = Option<i64>, Query, description = "Page size (default 50, max 200)"),
    ),
    responses(
        (status = 200, description = "Paginated message list"),
        (status = 401, description = "Authentication required"),
        (status = 404, description = "Session not found or feature disabled"),
    ),
    tag = "chat"
)]
pub async fn list_chat_messages(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Query(params): Query<ListMessagesParams>,
    auth: AuthContext,
) -> Result<impl IntoResponse, AppError> {
    if !state.config.chat_panel_enabled {
        return Err(AppError::ChatFeatureDisabled);
    }

    let caller = require_chat_auth(auth)?;

    // Verify the session exists and belongs to this tenant (tenant isolation).
    db_get_chat_session(&state.pool, session_id, caller.tenant_id).await?;

    // Clamp page size: [1, 200]. All values fit in both i64 and usize.
    let limit_i64: i64 = params.limit.unwrap_or(50).clamp(1, 200);
    #[allow(clippy::cast_sign_loss)]
    let limit_usize: usize = limit_i64 as usize;
    // Fetch one extra row to determine has_more without a COUNT query.
    let rows = db_list_chat_messages(
        &state.pool,
        session_id,
        caller.tenant_id,
        limit_i64 + 1,
        params.after_seq,
    )
    .await?;

    let has_more = rows.len() > limit_usize;
    let messages: Vec<ChatMessageDto> = rows
        .into_iter()
        .take(limit_usize)
        .map(ChatMessageDto::from)
        .collect();

    Ok(Json(ListMessagesResponse { messages, has_more }))
}
