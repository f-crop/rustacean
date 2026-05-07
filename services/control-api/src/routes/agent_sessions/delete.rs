use axum::{
    extract::{Path, State},
    http::StatusCode,
};
use rb_kafka::EventEnvelope;
use rb_schemas::{AgentCommand, SessionTerminate};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{require_session, AuthContext},
    state::AppState,
};

pub async fn delete_session(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let session = require_session(&auth)?;
    let tenant_id = session.tenant_id;

    let row = sqlx::query(
        r#"
        SELECT status FROM agent_sessions WHERE id = $1 AND tenant_id = $2
        "#,
    )
    .bind(session_id)
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Database)?;

    let _row = row.ok_or(AppError::NotFound)?;

    let command = AgentCommand {
        tenant_id: tenant_id.to_string(),
        event_id: Uuid::new_v4().to_string(),
        session_id: session_id.to_string(),
        runtime: 0,
        input_prompt: String::new(),
        workspace_path: String::new(),
        created_at_ms: chrono::Utc::now().timestamp_millis(),
        traceparent: String::new(),
        command: Some(rb_schemas::agent_command::Command::Terminate(SessionTerminate {
            force: false,
        })),
    };

    let envelope = EventEnvelope::new(tenant_id.into(), command);

    let producer = state
        .agent_producer
        .as_ref()
        .ok_or(AppError::KafkaNotConfigured)?;

    producer
        .publish("rb.agent.commands", session_id.to_string().as_bytes(), envelope)
        .await
        .map_err(AppError::KafkaPublish)?;

    Ok(StatusCode::ACCEPTED)
}
