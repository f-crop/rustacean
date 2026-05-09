use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use metrics::counter;
use rb_kafka::{Consumer, EventEnvelope, Producer, ProducerCfg};
use rb_schemas::{
    AgentErrorCategory, AgentEvent, AgentEventKind, AgentSessionCommand, TenantId,
    agent_session_command,
};
use tokio::task::JoinHandle;

use crate::session::{SessionManager, spawn_workspace_gc};

pub const TOPIC_AGENT_COMMANDS: &str = "rb.agent.commands";
const TOPIC_AGENT_EVENTS: &str = "rb.agent.events";

pub struct ConsumerCtx {
    session_manager: Arc<SessionManager>,
    producer: Arc<Producer<AgentEvent>>,
}

pub fn spawn(
    consumer: Consumer<AgentSessionCommand>,
    workspace_base: PathBuf,
    control_api_base: String,
    http_client: reqwest::Client,
) -> Result<JoinHandle<()>> {
    let session_manager = Arc::new(SessionManager::new(
        workspace_base.clone(),
        control_api_base,
        http_client,
    ));
    let producer = Arc::new(Producer::<AgentEvent>::new(&ProducerCfg::default())?);

    spawn_workspace_gc(workspace_base);

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<(TenantId, AgentEvent)>(1000);

    let producer_clone = producer.clone();
    tokio::spawn(async move {
        while let Some((_tenant_id, event)) = event_rx.recv().await {
            let key = format!("{}.{}", event.session_id, event.seq);
            // H8: Avoid unwrap by using proper error handling for tenant_id parsing
            let tenant_id: TenantId = match event.tenant_id.parse() {
                Ok(id) => id,
                Err(e) => {
                    tracing::error!(session_id = %event.session_id, error = %e, "Failed to parse tenant_id from event");
                    continue;
                }
            };
            let envelope = EventEnvelope::new(tenant_id, event);
            if let Err(e) = producer_clone
                .publish(TOPIC_AGENT_EVENTS, key.as_bytes(), envelope)
                .await
            {
                tracing::error!("Failed to publish agent event: {e}");
                counter!("rb_agent_events_failed_total").increment(1);
            } else {
                counter!("rb_agent_events_published_total").increment(1);
            }
        }
    });

    let ctx = Arc::new(ConsumerCtx {
        session_manager,
        producer,
    });

    let handle = tokio::spawn({
        let event_tx = event_tx.clone();
        async move {
            loop {
                match consumer.next().await {
                    None => {
                        tracing::info!("agent-runner: consumer stream ended");
                        break;
                    }
                    Some(Err(e)) => {
                        tracing::error!("agent-runner: kafka error: {e}");
                        counter!("rb_agent_commands_errors_total").increment(1);
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                    Some(Ok(envelope)) => {
                        handle_command(&ctx, &consumer, envelope, event_tx.clone()).await;
                    }
                }
            }
        }
    });

    Ok(handle)
}

async fn handle_command(
    ctx: &Arc<ConsumerCtx>,
    consumer: &Consumer<AgentSessionCommand>,
    envelope: EventEnvelope<AgentSessionCommand>,
    event_sender: tokio::sync::mpsc::Sender<(TenantId, AgentEvent)>,
) {
    let cmd = &envelope.payload;
    let tenant_id = envelope.tenant_id;
    let session_id = cmd.session_id.clone();

    let result = match &cmd.command {
        Some(agent_session_command::Command::Start(start)) => {
            ctx.session_manager
                .start_session(start, tenant_id, &session_id, event_sender)
                .await
        }
        Some(agent_session_command::Command::Input(input)) => {
            ctx.session_manager.send_input(&session_id, input).await
        }
        Some(agent_session_command::Command::Terminate(terminate)) => {
            ctx.session_manager
                .terminate_session(&session_id, terminate, event_sender)
                .await
        }
        None => Err(anyhow::anyhow!("Empty command")),
    };

    match result {
        Ok(()) => {
            counter!("rb_agent_commands_total", "outcome" => "ok").increment(1);
            if let Err(e) = consumer.commit(&envelope).await {
                tracing::warn!(session_id = %session_id, "Commit failed: {e}");
            }
        }
        Err(e) => {
            tracing::error!(session_id = %session_id, "Command failed: {e}");
            counter!("rb_agent_commands_total", "outcome" => "err").increment(1);
            emit_error_event(&ctx.producer, tenant_id, &session_id, &e).await;
            // H1: Commit the offset even on unrecoverable errors to prevent infinite retry.
            // The error has been logged and emitted as an event; dropping the message
            // here would cause Kafka to redeliver it forever, blocking the partition.
            if let Err(commit_err) = consumer.commit(&envelope).await {
                tracing::warn!(session_id = %session_id, "Commit after error failed: {commit_err}");
            }
        }
    }
}

async fn emit_error_event(
    producer: &Producer<AgentEvent>,
    tenant_id: TenantId,
    session_id: &str,
    error: &anyhow::Error,
) {
    // H4: Error events use i64::MIN + 1, distinct from terminated (i64::MIN + 2)
    const ERROR_SEQ: i64 = i64::MIN + 1;

    let payload = serde_json::json!({
        "message": error.to_string(),
        "category": AgentErrorCategory::SpawnFailed as i32
    });
    let event = AgentEvent {
        tenant_id: tenant_id.to_string(),
        session_id: session_id.to_string(),
        seq: ERROR_SEQ,
        kind: AgentEventKind::Error.into(),
        payload: payload.to_string(),
        emitted_at_ms: chrono::Utc::now().timestamp_millis(),
    };
    let key = format!("{}.{}", session_id, event.seq);
    let envelope = EventEnvelope::new(tenant_id, event);
    if let Err(e) = producer
        .publish(TOPIC_AGENT_EVENTS, key.as_bytes(), envelope)
        .await
    {
        tracing::error!("Failed to publish error event: {e}");
    }
}
