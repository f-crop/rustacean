use axum::{extract::State, Json};
use rb_kafka::EventEnvelope;
use rb_schemas::{AgentCommand, AgentRuntime, SessionStart};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{require_session, AuthContext},
    state::AppState,
};

const MAX_INPUT_PROMPT_LENGTH: usize = 1000;

#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    pub runtime: String,
    pub input_prompt: String,
}

#[derive(Debug, Serialize)]
pub struct CreateSessionResponse {
    pub session_id: String,
    pub status: String,
}

pub async fn create_session(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<Json<CreateSessionResponse>, AppError> {
    let session = require_session(&auth)?;
    let tenant_id = session.tenant_id;
    let user_id = session.user_id;

    let runtime = parse_runtime(&req.runtime)?;

    let input_prompt = if req.input_prompt.len() > MAX_INPUT_PROMPT_LENGTH {
        req.input_prompt[..MAX_INPUT_PROMPT_LENGTH].to_string()
    } else {
        req.input_prompt
    };

    let agent_permit = state
        .agent_registry
        .try_acquire()
        .ok_or(AppError::SessionCapExceeded)?;

    let session_id = Uuid::new_v4();
    let workspace_path = format!("/data/workspaces/{}/{}", tenant_id, session_id);

    let span_id = Uuid::new_v4();
    let traceparent = format!(
        "00-{:032x}-{:016x}-01",
        session_id.as_u128(),
        span_id.as_u128() & 0xffffffffffffffff
    );

    let mut tx = state.pool.begin().await.map_err(AppError::Database)?;

    sqlx::query(
        r#"
        INSERT INTO agent_sessions (id, tenant_id, created_by, runtime, input_prompt, workspace_path, status, trace_id)
        VALUES ($1, $2, $3, $4, $5, $6, 'pending', $7)
        "#,
    )
    .bind(session_id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(format!("{:?}", runtime).to_lowercase())
    .bind(&input_prompt)
    .bind(&workspace_path)
    .bind(&traceparent)
    .execute(&mut *tx)
    .await
    .map_err(AppError::Database)?;

    let api_key = format!("rb_session_{}_{}", tenant_id, Uuid::new_v4());
    let command_event_id = Uuid::new_v4();

    let command = AgentCommand {
        tenant_id: tenant_id.to_string(),
        event_id: command_event_id.to_string(),
        session_id: session_id.to_string(),
        runtime: runtime as i32,
        input_prompt,
        workspace_path: workspace_path.clone(),
        created_at_ms: chrono::Utc::now().timestamp_millis(),
        traceparent: traceparent.clone(),
        command: Some(rb_schemas::agent_command::Command::Start(SessionStart {
            api_key,
        })),
    };

    let envelope = EventEnvelope::new(tenant_id.into(), command)
        .with_event_id(command_event_id)
        .with_trace_context(rb_kafka::TraceContext {
            traceparent: traceparent.clone(),
            tracestate: String::new(),
        });

    let producer = state
        .agent_producer
        .as_ref()
        .ok_or(AppError::KafkaNotConfigured)?;

    producer
        .publish("rb.agent.commands", session_id.to_string().as_bytes(), envelope)
        .await
        .map_err(AppError::KafkaPublish)?;

    tx.commit().await.map_err(AppError::Database)?;

    drop(agent_permit);

    Ok(Json(CreateSessionResponse {
        session_id: session_id.to_string(),
        status: "pending".to_string(),
    }))
}

fn parse_runtime(s: &str) -> Result<AgentRuntime, AppError> {
    match s.to_lowercase().as_str() {
        "claude_code" | "claude" => Ok(AgentRuntime::ClaudeCode),
        "opencode" => Ok(AgentRuntime::Opencode),
        "pi" => Ok(AgentRuntime::Pi),
        _ => Err(AppError::InvalidInput),
    }
}
