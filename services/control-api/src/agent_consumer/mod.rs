//! Agent event consumer — consumes `rb.agent.events` from Kafka,
//! persists to `agents.agent_events`, and fans out via SSE.
//!
//! Architecture:
//! - Consumes [`AgentEvent`] protobuf messages from Kafka
//! - Writes events to Postgres `agents.agent_events` table (partitioned)
//! - Publishes to SSE bus for live streaming to clients
//! - Preserves trace context for distributed tracing

mod db;
mod sse;

use std::sync::{
    Arc,
    atomic::{AtomicI64, AtomicU64, Ordering},
};

use anyhow::Result;
use rb_kafka::{Consumer, ConsumerCfg};
use rb_schemas::{AgentEvent, TenantId};
use rb_sse::EventBus;
use sqlx::PgPool;

use crate::state::AgentRegistry;

/// Spawn the long-running Kafka consumer task that subscribes to
/// `rb.agent.events`, persists events to Postgres, and fans events
/// out through the SSE bus.
///
/// Returns the [`tokio::task::JoinHandle`] so the caller can abort on shutdown.
///
/// # Errors
///
/// Returns an error if the Kafka consumer cannot be created or subscribed.
pub fn spawn(
    cfg: &ConsumerCfg,
    sse_bus: Arc<EventBus>,
    pool: Arc<PgPool>,
    agent_registry: Arc<AgentRegistry>,
    last_event_at_ms: Arc<AtomicI64>,
    lag_records: Arc<AtomicU64>,
) -> Result<tokio::task::JoinHandle<()>> {
    let consumer = Consumer::<AgentEvent>::new(cfg)?;
    consumer.subscribe(&["rb.agent.events"])?;

    let handle = tokio::spawn(async move {
        loop {
            match consumer.next().await {
                None => {
                    tracing::info!("agent_consumer: stream ended");
                    break;
                }
                Some(Err(e)) => {
                    tracing::error!("agent_consumer: kafka error: {e}");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
                Some(Ok(envelope)) => {
                    let now_ms = chrono::Utc::now().timestamp_millis();
                    last_event_at_ms.store(now_ms, Ordering::Relaxed);
                    lag_records.store(0, Ordering::Relaxed);

                    let ev = &envelope.payload;
                    
                    // Parse tenant_id from the event
                    let tenant_id = match ev.tenant_id.parse::<uuid::Uuid>() {
                        Ok(id) => TenantId::from(id),
                        Err(e) => {
                            tracing::error!("agent_consumer: invalid tenant_id: {e}");
                            continue;
                        }
                    };

                    // Persist to database
                    if let Err(e) = db::persist_event(&pool, ev).await {
                        tracing::error!(
                            session_id = %ev.session_id,
                            "agent_consumer: DB persist failed: {e}"
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        continue;
                    }

                    // Update agent registry if this is a terminal event
                    if let Err(e) = update_registry(&agent_registry, ev).await {
                        tracing::warn!(
                            session_id = %ev.session_id,
                            "agent_consumer: registry update failed: {e}"
                        );
                    }

                    // Fan out to SSE bus
                    let json_ev = match sse::AgentEventJson::from_proto(ev) {
                        Ok(j) => j,
                        Err(e) => {
                            tracing::error!("agent_consumer: failed to convert event: {e}");
                            continue;
                        }
                    };

                    match serde_json::to_string(&json_ev) {
                        Ok(data) => {
                            sse_bus.publish_raw(&tenant_id, "agent.event", data);
                        }
                        Err(e) => {
                            tracing::error!("agent_consumer: serialize error: {e}");
                        }
                    }

                    // Commit offset
                    if let Err(e) = consumer.commit(&envelope).await {
                        tracing::warn!("agent_consumer: commit failed: {e}");
                    }
                }
            }
        }
    });

    Ok(handle)
}

/// Update agent registry when terminal events are received.
async fn update_registry(registry: &AgentRegistry, ev: &AgentEvent) -> Result<()> {
    use rb_schemas::AgentEventType;
    
    let event_type = AgentEventType::try_from(ev.event_type)
        .unwrap_or(AgentEventType::Unspecified);
    
    if event_type == AgentEventType::System {
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(&ev.event_data_json) {
            if let Some(status_val) = data.get("status").and_then(|v| v.as_i64()) {
                use rb_schemas::AgentSessionStatus;
                
                let is_terminal = match AgentSessionStatus::try_from(status_val as i32) {
                    Ok(AgentSessionStatus::Completed) |
                    Ok(AgentSessionStatus::Failed) |
                    Ok(AgentSessionStatus::Terminated) => true,
                    _ => false,
                };
                
                if is_terminal {
                    if let Ok(session_id) = ev.session_id.parse() {
                        registry.remove(&session_id);
                        tracing::debug!(
                            session_id = %ev.session_id,
                            status = status_val,
                            "Removed session from registry"
                        );
                    }
                }
            }
        }
    }
    
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rb_schemas::{AgentEventType, AgentSessionStatus};

    #[test]
    fn agent_event_type_converts_correctly() {
        assert_eq!(AgentEventType::System as i32, 1);
        assert_eq!(AgentEventType::Stdout as i32, 2);
        assert_eq!(AgentEventType::Stderr as i32, 3);
        assert_eq!(AgentEventType::ToolCall as i32, 4);
        assert_eq!(AgentEventType::ToolResult as i32, 5);
    }

    #[test]
    fn terminal_statuses_are_detected() {
        // Test that Completed, Failed, and Terminated status values are detected correctly
        // These status values correspond to the protobuf values:
        // Completed = 3, Failed = 4, Terminated = 5
        let terminal_statuses = [3, 4, 5];
        
        for status_val in terminal_statuses {
            let is_terminal = match AgentSessionStatus::try_from(status_val) {
                Ok(AgentSessionStatus::Completed) |
                Ok(AgentSessionStatus::Failed) |
                Ok(AgentSessionStatus::Terminated) => true,
                _ => false,
            };
            assert!(is_terminal, "status {} should be terminal", status_val);
        }
        
        // Non-terminal statuses should not be detected as terminal
        let non_terminal_statuses = [0, 1, 2]; // Unspecified, Pending, Running
        for status_val in non_terminal_statuses {
            let is_terminal = match AgentSessionStatus::try_from(status_val) {
                Ok(AgentSessionStatus::Completed) |
                Ok(AgentSessionStatus::Failed) |
                Ok(AgentSessionStatus::Terminated) => true,
                _ => false,
            };
            assert!(!is_terminal, "status {} should not be terminal", status_val);
        }
    }
}
