//! Kafka consumer loop for `rb.ingest.graph.commands`.
//!
//! Consumes [`GraphRelationEvent`] protobuf messages and writes nodes/edges to
//! Neo4j via [`TenantGraph`].  Multi-statement Cypher is rejected by the
//! storage crate (ADR-007 §11.10 / REQ-IN-14).

use std::sync::Arc;

use anyhow::Result;
use metrics::counter;
use rb_kafka::{Consumer, ConsumerCfg, EventEnvelope, Producer, ProducerCfg};
use rb_schemas::{GraphRelationEvent, IngestStage, IngestStatus, IngestStatusEvent, RelationKind};
use rb_storage_neo4j::{CypherError, TenantGraph};
use tokio::task::JoinHandle;

use crate::writer::write_relation;

const TOPIC_COMMANDS: &str = "rb.ingest.graph.commands";
const TOPIC_PROJECTOR_EVENTS: &str = "rb.projector.events";
const MONOMORPH_NODE_CAP_DEFAULT: i64 = 5_000_000;

/// Spawn the long-running consumer task.
///
/// Returns the [`JoinHandle`] so the caller can abort on shutdown.
///
/// # Errors
///
/// Returns an error if the Kafka consumer cannot be created or subscribed.
pub fn spawn(graph: Arc<TenantGraph>) -> Result<JoinHandle<()>> {
    let consumer = Consumer::<GraphRelationEvent>::new(&ConsumerCfg::new("projector-neo4j"))?;
    consumer.subscribe(&[TOPIC_COMMANDS])?;

    let producer = Arc::new(Producer::<IngestStatusEvent>::new(&ProducerCfg::default())?);
    let monomorph_cap = monomorph_cap_from_env();

    let handle = tokio::spawn(async move {
        loop {
            match consumer.next().await {
                None => {
                    tracing::info!("projector_neo4j: stream ended");
                    break;
                }
                Some(Err(e)) => {
                    tracing::error!("projector_neo4j: kafka error: {e}");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
                Some(Ok(envelope)) => {
                    handle_envelope(
                        graph.as_ref(),
                        &consumer,
                        producer.as_ref(),
                        monomorph_cap,
                        envelope,
                    )
                    .await;
                }
            }
        }
    });

    Ok(handle)
}

async fn handle_envelope(
    graph: &TenantGraph,
    consumer: &Consumer<GraphRelationEvent>,
    producer: &Producer<IngestStatusEvent>,
    monomorph_cap: i64,
    envelope: EventEnvelope<GraphRelationEvent>,
) {
    let ev = &envelope.payload;
    let tenant_id = envelope.tenant_id;
    let ingest_run_id = ev.ingest_run_id.clone();

    // Cap enforcement for TypeInstance nodes (ADR-007 §13.7).
    let kind = RelationKind::try_from(ev.kind)
        .unwrap_or(RelationKind::Unspecified);
    if matches!(
        kind,
        RelationKind::MonomorphizedFrom | RelationKind::TypeArgBinds
    ) {
        match graph.count_type_instances(&tenant_id).await {
            Ok(cnt) if cnt >= monomorph_cap => {
                tracing::error!(
                    tenant_id = %tenant_id,
                    count = cnt,
                    cap = monomorph_cap,
                    "projector_neo4j: monomorph_cap_exceeded — DLQing event"
                );
                counter!("rb_projector_neo4j_cap_exceeded_total").increment(1);
                emit_failed_status(producer, &tenant_id, &ingest_run_id, "monomorph_cap_exceeded")
                    .await;
                // Commit so the consumer does not redeliver the capped event indefinitely.
                // Operator must bump RB_MONOMORPH_NODE_CAP to resume processing.
                if let Err(e) = consumer.commit(&envelope).await {
                    tracing::warn!("projector_neo4j: commit failed after cap: {e}");
                }
                return;
            }
            Err(e) => {
                tracing::error!(
                    tenant_id = %tenant_id,
                    "projector_neo4j: cap-count query failed (transient): {e}"
                );
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                return;
            }
            Ok(_) => {}
        }
    }

    // Write the relation to Neo4j.
    match write_relation(graph, &tenant_id, ev).await {
        Ok(()) => {
            counter!(
                "rb_projector_neo4j_events_total",
                "outcome" => "ok"
            )
            .increment(1);
            tracing::debug!(
                tenant_id = %tenant_id,
                from_fqn = %ev.from_fqn,
                to_fqn = %ev.to_fqn,
                kind = ev.kind,
                "projector_neo4j: relation written"
            );
            emit_done_status(producer, &tenant_id, &ingest_run_id).await;
            if let Err(e) = consumer.commit(&envelope).await {
                tracing::warn!("projector_neo4j: commit failed: {e}");
            }
        }
        Err(CypherError::MultiStatement) => {
            // Terminal security rejection — DLQ immediately.
            tracing::error!(
                tenant_id = %tenant_id,
                from_fqn = %ev.from_fqn,
                "projector_neo4j: multi-statement Cypher rejected — DLQing"
            );
            counter!(
                "rb_projector_neo4j_events_total",
                "outcome" => "dlq_multi_statement"
            )
            .increment(1);
            if let Err(e) = consumer.nack_to_dlq(&envelope, "multi-statement-cypher").await {
                tracing::error!("projector_neo4j: DLQ failed: {e}");
            }
            if let Err(e) = consumer.commit(&envelope).await {
                tracing::warn!("projector_neo4j: commit failed after DLQ: {e}");
            }
        }
        Err(e) => {
            // Transient failure — back off, do not commit so Kafka redelivers.
            tracing::error!(
                tenant_id = %tenant_id,
                from_fqn = %ev.from_fqn,
                "projector_neo4j: write failed (transient): {e}"
            );
            counter!(
                "rb_projector_neo4j_events_total",
                "outcome" => "err"
            )
            .increment(1);
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
}

/// Emit `IngestStatusEvent{stage=ProjectNeo4j, status=Done}` to `rb.projector.events`.
async fn emit_done_status(
    producer: &Producer<IngestStatusEvent>,
    tenant_id: &rb_schemas::TenantId,
    ingest_run_id: &str,
) {
    let status_ev = IngestStatusEvent {
        ingest_request_id: String::new(),
        tenant_id: tenant_id.to_string(),
        status: IngestStatus::Done as i32,
        error_message: String::new(),
        occurred_at_ms: chrono::Utc::now().timestamp_millis(),
        stage: IngestStage::ProjectNeo4j as i32,
        stage_seq: 8,
        ingest_run_id: ingest_run_id.to_owned(),
        attempt: 0,
    };
    let env = EventEnvelope::new(*tenant_id, status_ev);
    let key = tenant_id.to_string();
    if let Err(e) = producer.publish(TOPIC_PROJECTOR_EVENTS, key.as_bytes(), env).await {
        tracing::error!("projector_neo4j: failed to publish done status event: {e}");
    }
}

/// Emit `IngestStatusEvent{stage=ProjectNeo4j, status=Failed}` to `rb.projector.events`.
async fn emit_failed_status(
    producer: &Producer<IngestStatusEvent>,
    tenant_id: &rb_schemas::TenantId,
    ingest_run_id: &str,
    error_message: &str,
) {
    let status_ev = IngestStatusEvent {
        ingest_request_id: String::new(),
        tenant_id: tenant_id.to_string(),
        status: IngestStatus::Failed as i32,
        error_message: error_message.to_owned(),
        occurred_at_ms: chrono::Utc::now().timestamp_millis(),
        stage: IngestStage::ProjectNeo4j as i32,
        stage_seq: 8,
        ingest_run_id: ingest_run_id.to_owned(),
        attempt: 1,
    };
    let env = EventEnvelope::new(*tenant_id, status_ev);
    let key = tenant_id.to_string();
    if let Err(e) = producer.publish(TOPIC_PROJECTOR_EVENTS, key.as_bytes(), env).await {
        tracing::error!("projector_neo4j: failed to publish status event: {e}");
    }
}

fn monomorph_cap_from_env() -> i64 {
    std::env::var("RB_MONOMORPH_NODE_CAP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(MONOMORPH_NODE_CAP_DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn monomorph_cap_default() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("RB_MONOMORPH_NODE_CAP") };
        assert_eq!(monomorph_cap_from_env(), MONOMORPH_NODE_CAP_DEFAULT);
    }

    #[test]
    fn monomorph_cap_from_env_var() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("RB_MONOMORPH_NODE_CAP", "1000000") };
        assert_eq!(monomorph_cap_from_env(), 1_000_000);
        unsafe { std::env::remove_var("RB_MONOMORPH_NODE_CAP") };
    }

    #[test]
    fn monomorph_cap_invalid_falls_back_to_default() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("RB_MONOMORPH_NODE_CAP", "not-a-number") };
        assert_eq!(monomorph_cap_from_env(), MONOMORPH_NODE_CAP_DEFAULT);
        unsafe { std::env::remove_var("RB_MONOMORPH_NODE_CAP") };
    }

    #[test]
    fn monomorph_cap_zero_is_accepted() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("RB_MONOMORPH_NODE_CAP", "0") };
        assert_eq!(monomorph_cap_from_env(), 0);
        unsafe { std::env::remove_var("RB_MONOMORPH_NODE_CAP") };
    }
}
