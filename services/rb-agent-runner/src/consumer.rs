use rb_kafka::{Consumer, Producer, ProducerCfg};
use rb_schemas::{AgentCommand, AgentEvent, AgentEventType, AgentSessionStatus, SystemEventData};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::adapter::AdapterError;
use crate::session_manager::SessionManager;
use crate::workspace::WorkspaceManager;

pub const TOPIC_AGENT_COMMANDS: &str = "rb.agent.commands";
pub const TOPIC_AGENT_EVENTS: &str = "rb.agent.events";

pub struct ConsumerContext {
    pub session_manager: Arc<SessionManager>,
    pub workspace_manager: Arc<WorkspaceManager>,
    pub db_pool: PgPool,
    pub event_producer: Arc<Producer<AgentEvent>>,
    pub control_api_url: String,
}

pub async fn run(
    consumer: Consumer<AgentCommand>,
    ctx: Arc<ConsumerContext>,
) {
    loop {
        match consumer.next().await {
            Some(Ok(envelope)) => {
                if let Err(e) = handle_command(&ctx, &envelope.payload, &envelope).await {
                    tracing::error!("Failed to handle agent command: {}", e);
                }
                if let Err(e) = consumer.commit(&envelope).await {
                    tracing::error!("Failed to commit offset: {}", e);
                }
            }
            Some(Err(e)) => {
                tracing::error!("Kafka consumer error: {}", e);
            }
            None => {
                tracing::info!("Kafka consumer stream ended");
                break;
            }
        }
    }
}

async fn handle_command(
    ctx: &ConsumerContext,
    cmd: &AgentCommand,
    envelope: &rb_kafka::EventEnvelope<AgentCommand>,
) -> Result<(), AdapterError> {
    use rb_schemas::agent_command::Command;

    let session_id = Uuid::parse_str(&cmd.session_id)
        .map_err(|e| AdapterError::SpawnFailed(format!("Invalid session_id: {}", e)))?;
    let tenant_id = Uuid::parse_str(&cmd.tenant_id)
        .map_err(|e| AdapterError::SpawnFailed(format!("Invalid tenant_id: {}", e)))?;

    match &cmd.command {
        Some(Command::Start(start)) => {
            tracing::info!("Starting agent session: {}", session_id);

            let ws_id = ctx.session_manager.create_session(cmd, &ctx.workspace_manager).await?;
            let workspace_path = ctx.workspace_manager.create_workspace(&tenant_id, &ws_id).await?;

            ctx.workspace_manager
                .write_mcp_config(&workspace_path, &start.api_key, &ctx.control_api_url)
                .await?;

            if cmd.runtime == rb_schemas::AgentRuntime::Opencode as i32 {
                ctx.workspace_manager.write_opencode_config(&workspace_path).await?;
            }

            tokio::fs::write(workspace_path.join("prompt.txt"), &cmd.input_prompt)
                .await
                .map_err(|e| AdapterError::Io(e))?;

            ctx.session_manager.start_session(ws_id, start).await?;

            update_session_status(&ctx.db_pool, &session_id, AgentSessionStatus::Running, None).await?;

            emit_event(
                ctx,
                &tenant_id,
                &session_id,
                AgentEventType::System,
                &SystemEventData {
                    message: "Session started".to_string(),
                    status: AgentSessionStatus::Running,
                },
                cmd.traceparent.clone(),
            )
            .await?;
        }
        Some(Command::Terminate(term)) => {
            tracing::info!("Terminating agent session: {}", session_id);

            ctx.session_manager.terminate_session(session_id, term).await?;

            update_session_status(
                &ctx.db_pool,
                &session_id,
                AgentSessionStatus::Terminated,
                if term.force { Some("forced") } else { Some("graceful") },
            )
            .await?;

            emit_event(
                ctx,
                &tenant_id,
                &session_id,
                AgentEventType::System,
                &SystemEventData {
                    message: "Session terminated".to_string(),
                    status: AgentSessionStatus::Terminated,
                },
                cmd.traceparent.clone(),
            )
            .await?;
        }
        Some(Command::Input(input)) => {
            tracing::info!("Sending input to agent session: {}", session_id);
            ctx.session_manager.send_input(session_id, input).await?;
        }
        None => {
            tracing::warn!("Received agent command with no command type");
        }
    }

    Ok(())
}

async fn update_session_status(
    pool: &PgPool,
    session_id: &Uuid,
    status: AgentSessionStatus,
    reason: Option<&str>,
) -> Result<(), AdapterError> {
    let status_str = format!("{:?}", status).to_lowercase();

    sqlx::query(
        r#"
        UPDATE agent_sessions
        SET status = $1::agent_session_status,
            status_reason = $2,
            started_at = CASE WHEN $1::agent_session_status = 'running' AND started_at IS NULL THEN NOW() ELSE started_at END,
            ended_at = CASE WHEN $1::agent_session_status IN ('completed', 'failed', 'terminated') THEN NOW() ELSE ended_at END
        WHERE id = $3
        "#,
    )
    .bind(&status_str)
    .bind(reason)
    .bind(session_id)
    .execute(pool)
    .await
    .map_err(|e| AdapterError::SpawnFailed(format!("DB error: {}", e)))?;

    Ok(())
}

async fn emit_event<E: serde::Serialize>(
    ctx: &ConsumerContext,
    tenant_id: &Uuid,
    session_id: &Uuid,
    event_type: AgentEventType,
    data: &E,
    traceparent: String,
) -> Result<(), AdapterError> {
    let event = AgentEvent {
        tenant_id: tenant_id.to_string(),
        event_id: Uuid::new_v4().to_string(),
        session_id: session_id.to_string(),
        event_type: event_type as i32,
        event_data_json: serde_json::to_string(data).map_err(|e| AdapterError::JsonParse(e))?,
        occurred_at_ms: chrono::Utc::now().timestamp_millis(),
        trace_id: if traceparent.is_empty() {
            None
        } else {
            traceparent.split('-').next().map(|s| s.to_string())
        },
        span_id: None,
    };

    ctx.event_producer
        .send(
            TOPIC_AGENT_EVENTS,
            tenant_id.to_string().as_bytes(),
            &event,
        )
        .await
        .map_err(|e| AdapterError::SpawnFailed(format!("Failed to emit event: {}", e)))?;

    Ok(())
}
