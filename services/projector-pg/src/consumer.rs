//! Kafka consumer loop for `projector-pg`.
//!
//! Consumes typed payload events and writes to per-tenant `PostgreSQL` tables
//! via `TenantPool`. Emits `IngestStatusEvent` per stage completion.

use std::sync::Arc;

use anyhow::Result;
use metrics::counter;
use rb_kafka::{Consumer, ConsumerCfg, EventEnvelope, Producer, ProducerCfg};
use rb_kafka_health::{HealthyConsumer, KafkaHealthWatcher, WatchdogConfig};
use rb_schemas::{
    GraphRelationEvent, IngestStage, IngestStatus, IngestStatusEvent, ParsedItemEvent,
    SourceFileEvent,
};
use rb_storage_pg::TenantPool;
use rb_tenant::TenantCtx;
use tokio::task::JoinHandle;

use crate::projection;

const TOPIC_SOURCE_FILE: &str = "rb.source-files.v1";
const TOPIC_PARSED_ITEMS: &str = "rb.parsed-items.v1";
const TOPIC_GRAPH_RELATIONS: &str = "rb.ingest.graph.commands";
const TOPIC_PROJECTOR_EVENTS: &str = "rb.projector.events";

/// Spawn the long-running consumer task.
///
/// Returns the [`JoinHandle`] so the caller can abort on shutdown.
#[allow(clippy::missing_errors_doc)]
pub fn spawn(pool: Arc<TenantPool>) -> Result<JoinHandle<()>> {
    // Source file consumer (from clone stage)
    let source_cfg = ConsumerCfg::new("projector-pg-source");
    let source_consumer = Consumer::<SourceFileEvent>::new(&source_cfg)?;
    source_consumer.subscribe(&[TOPIC_SOURCE_FILE])?;
    let mut source_consumer = KafkaHealthWatcher::wrap(
        source_consumer,
        &source_cfg,
        &[TOPIC_SOURCE_FILE.to_owned()],
        WatchdogConfig::default(),
    );

    // Parsed item consumer (from parse stage)
    let items_cfg = ConsumerCfg::new("projector-pg-items");
    let item_consumer = Consumer::<ParsedItemEvent>::new(&items_cfg)?;
    item_consumer.subscribe(&[TOPIC_PARSED_ITEMS])?;
    let mut item_consumer = KafkaHealthWatcher::wrap(
        item_consumer,
        &items_cfg,
        &[TOPIC_PARSED_ITEMS.to_owned()],
        WatchdogConfig::default(),
    );

    // Relation consumer (from graph stage)
    let relations_cfg = ConsumerCfg::new("projector-pg-relations");
    let relation_consumer = Consumer::<GraphRelationEvent>::new(&relations_cfg)?;
    relation_consumer.subscribe(&[TOPIC_GRAPH_RELATIONS])?;
    let mut relation_consumer = KafkaHealthWatcher::wrap(
        relation_consumer,
        &relations_cfg,
        &[TOPIC_GRAPH_RELATIONS.to_owned()],
        WatchdogConfig::default(),
    );

    let producer = Arc::new(Producer::<IngestStatusEvent>::new(&ProducerCfg::default())?);

    let handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                result = source_consumer.next() => {
                    match result {
                        None => {
                            tracing::info!("projector_pg: source consumer stream ended");
                            break;
                        }
                        Some(Err(e)) => {
                            tracing::error!("projector_pg: source kafka error: {e}");
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        }
                        Some(Ok(envelope)) => {
                            handle_source_file(&pool, &source_consumer, producer.as_ref(), envelope).await;
                        }
                    }
                }
                result = item_consumer.next() => {
                    match result {
                        None => {
                            tracing::info!("projector_pg: item consumer stream ended");
                            break;
                        }
                        Some(Err(e)) => {
                            tracing::error!("projector_pg: item kafka error: {e}");
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        }
                        Some(Ok(envelope)) => {
                            handle_parsed_item(&pool, &item_consumer, producer.as_ref(), envelope).await;
                        }
                    }
                }
                result = relation_consumer.next() => {
                    match result {
                        None => {
                            tracing::info!("projector_pg: relation consumer stream ended");
                            break;
                        }
                        Some(Err(e)) => {
                            tracing::error!("projector_pg: relation kafka error: {e}");
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        }
                        Some(Ok(envelope)) => {
                            handle_relation(&pool, &relation_consumer, producer.as_ref(), envelope).await;
                        }
                    }
                }
            }
        }
    });

    Ok(handle)
}

async fn handle_source_file(
    pool: &TenantPool,
    consumer: &HealthyConsumer<SourceFileEvent>,
    producer: &Producer<IngestStatusEvent>,
    envelope: EventEnvelope<SourceFileEvent>,
) {
    let ev = &envelope.payload;
    let tenant_id = envelope.tenant_id;
    let tenant_ctx = TenantCtx::new(tenant_id);
    let ingest_run_id = ev.ingest_run_id.clone();

    emit_processing_status(producer, &tenant_id, &ingest_run_id).await;

    match projection::write_source_file(pool, &tenant_ctx, &envelope.tenant_id, ev).await {
        Ok(()) => {
            counter!("rb_projector_pg_events_total", "event_type" => "source_file", "outcome" => "ok")
                .increment(1);
            if let Err(e) = consumer.commit(&envelope).await {
                tracing::warn!("projector_pg: source commit failed: {e}");
            }
            emit_ok_status(producer, &tenant_id, &ingest_run_id, IngestStage::ProjectPg).await;
        }
        Err(e) => {
            tracing::error!(
                tenant_id = %tenant_id,
                path = %ev.relative_path,
                "projector_pg: source write failed: {e}"
            );
            counter!("rb_projector_pg_events_total", "event_type" => "source_file", "outcome" => "err")
                .increment(1);
            emit_failed_status(
                producer,
                &tenant_id,
                &ingest_run_id,
                &format!("source_write_failed: {e}"),
            )
            .await;
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
}

async fn handle_parsed_item(
    pool: &TenantPool,
    consumer: &HealthyConsumer<ParsedItemEvent>,
    producer: &Producer<IngestStatusEvent>,
    envelope: EventEnvelope<ParsedItemEvent>,
) {
    let ev = &envelope.payload;
    let tenant_id = envelope.tenant_id;
    let tenant_ctx = TenantCtx::new(tenant_id);
    let ingest_run_id = ev.ingest_run_id.clone();

    emit_processing_status(producer, &tenant_id, &ingest_run_id).await;

    match projection::write_parsed_item(pool, &tenant_ctx, &envelope.tenant_id, ev).await {
        Ok(()) => {
            counter!("rb_projector_pg_events_total", "event_type" => "parsed_item", "outcome" => "ok")
                .increment(1);
            if let Err(e) = consumer.commit(&envelope).await {
                tracing::warn!("projector_pg: item commit failed: {e}");
            }
            emit_ok_status(producer, &tenant_id, &ingest_run_id, IngestStage::ProjectPg).await;
        }
        Err(e) => {
            tracing::error!(
                tenant_id = %tenant_id,
                fqn = %ev.fqn,
                "projector_pg: item write failed: {e}"
            );
            counter!("rb_projector_pg_events_total", "event_type" => "parsed_item", "outcome" => "err")
                .increment(1);
            emit_failed_status(
                producer,
                &tenant_id,
                &ingest_run_id,
                &format!("item_write_failed: {e}"),
            )
            .await;
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
}

async fn handle_relation(
    pool: &TenantPool,
    consumer: &HealthyConsumer<GraphRelationEvent>,
    producer: &Producer<IngestStatusEvent>,
    envelope: EventEnvelope<GraphRelationEvent>,
) {
    let ev = &envelope.payload;
    let tenant_id = envelope.tenant_id;
    let tenant_ctx = TenantCtx::new(tenant_id);
    let ingest_run_id = ev.ingest_run_id.clone();

    emit_processing_status(producer, &tenant_id, &ingest_run_id).await;

    match projection::write_relation(pool, &tenant_ctx, &envelope.tenant_id, ev).await {
        Ok(()) => {
            counter!("rb_projector_pg_events_total", "event_type" => "relation", "outcome" => "ok")
                .increment(1);
            if let Err(e) = consumer.commit(&envelope).await {
                tracing::warn!("projector_pg: relation commit failed: {e}");
            }
            emit_ok_status(producer, &tenant_id, &ingest_run_id, IngestStage::ProjectPg).await;
        }
        Err(e) => {
            tracing::error!(
                tenant_id = %tenant_id,
                from_fqn = %ev.from_fqn,
                to_fqn = %ev.to_fqn,
                "projector_pg: relation write failed: {e}"
            );
            counter!("rb_projector_pg_events_total", "event_type" => "relation", "outcome" => "err")
                .increment(1);
            emit_failed_status(
                producer,
                &tenant_id,
                &ingest_run_id,
                &format!("relation_write_failed: {e}"),
            )
            .await;
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
}

async fn emit_processing_status(
    producer: &Producer<IngestStatusEvent>,
    tenant_id: &rb_schemas::TenantId,
    ingest_run_id: &str,
) {
    let ev = IngestStatusEvent {
        ingest_request_id: String::new(),
        tenant_id: tenant_id.to_string(),
        status: IngestStatus::Processing as i32,
        error_message: String::new(),
        occurred_at_ms: chrono::Utc::now().timestamp_millis(),
        stage: IngestStage::ProjectPg as i32,
        stage_seq: 7,
        ingest_run_id: ingest_run_id.to_owned(),
        attempt: 0,
    };
    let env = EventEnvelope::new(*tenant_id, ev);
    let key = tenant_id.to_string();
    if let Err(e) = producer
        .publish(TOPIC_PROJECTOR_EVENTS, key.as_bytes(), env)
        .await
    {
        tracing::warn!("projector_pg: failed to publish processing status: {e}");
    }
}

fn build_ok_status_event(
    tenant_id: &rb_schemas::TenantId,
    ingest_run_id: &str,
    stage: IngestStage,
) -> IngestStatusEvent {
    IngestStatusEvent {
        ingest_request_id: String::new(),
        tenant_id: tenant_id.to_string(),
        status: IngestStatus::Done as i32,
        error_message: String::new(),
        occurred_at_ms: chrono::Utc::now().timestamp_millis(),
        stage: stage as i32,
        stage_seq: 0,
        ingest_run_id: ingest_run_id.to_owned(),
        attempt: 0,
    }
}

fn build_failed_status_event(
    tenant_id: &rb_schemas::TenantId,
    ingest_run_id: &str,
    error_message: &str,
) -> IngestStatusEvent {
    IngestStatusEvent {
        ingest_request_id: String::new(),
        tenant_id: tenant_id.to_string(),
        status: IngestStatus::Failed as i32,
        error_message: error_message.to_owned(),
        occurred_at_ms: chrono::Utc::now().timestamp_millis(),
        stage: IngestStage::ProjectPg as i32,
        stage_seq: 0,
        ingest_run_id: ingest_run_id.to_owned(),
        attempt: 0,
    }
}

async fn emit_ok_status(
    producer: &Producer<IngestStatusEvent>,
    tenant_id: &rb_schemas::TenantId,
    ingest_run_id: &str,
    stage: IngestStage,
) {
    let status_ev = build_ok_status_event(tenant_id, ingest_run_id, stage);
    let env = EventEnvelope::new(*tenant_id, status_ev);
    let key = tenant_id.to_string();
    if let Err(e) = producer
        .publish(TOPIC_PROJECTOR_EVENTS, key.as_bytes(), env)
        .await
    {
        tracing::error!("projector_pg: failed to publish ok status event: {e}");
    }
}

async fn emit_failed_status(
    producer: &Producer<IngestStatusEvent>,
    tenant_id: &rb_schemas::TenantId,
    ingest_run_id: &str,
    error_message: &str,
) {
    let status_ev = build_failed_status_event(tenant_id, ingest_run_id, error_message);
    let env = EventEnvelope::new(*tenant_id, status_ev);
    let key = tenant_id.to_string();
    if let Err(e) = producer
        .publish(TOPIC_PROJECTOR_EVENTS, key.as_bytes(), env)
        .await
    {
        tracing::error!("projector_pg: failed to publish status event: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rb_schemas::TenantId as SchemasTenantId;

    #[test]
    fn build_ok_status_event_sets_done_status() {
        let tid = SchemasTenantId::new();
        let run_id = "run-abc";
        let ev = build_ok_status_event(&tid, run_id, IngestStage::ProjectPg);
        assert_eq!(ev.status, IngestStatus::Done as i32);
        assert_eq!(ev.stage, IngestStage::ProjectPg as i32);
        assert_eq!(ev.ingest_run_id, run_id);
        assert_eq!(ev.tenant_id, tid.to_string());
        assert!(ev.error_message.is_empty());
        assert_eq!(
            ev.attempt, 0,
            "first attempt must be 0 per proto convention"
        );
    }

    #[test]
    fn build_ok_status_event_uses_provided_stage() {
        let tid = SchemasTenantId::new();
        let ev = build_ok_status_event(&tid, "run-1", IngestStage::Clone);
        assert_eq!(ev.stage, IngestStage::Clone as i32);
    }

    #[test]
    fn build_failed_status_event_sets_failed_status() {
        let tid = SchemasTenantId::new();
        let run_id = "run-def";
        let err = "something went wrong";
        let ev = build_failed_status_event(&tid, run_id, err);
        assert_eq!(ev.status, IngestStatus::Failed as i32);
        assert_eq!(ev.stage, IngestStage::ProjectPg as i32);
        assert_eq!(ev.ingest_run_id, run_id);
        assert_eq!(ev.error_message, err);
        assert_eq!(
            ev.attempt, 0,
            "first attempt must be 0 per proto convention"
        );
    }

    #[test]
    fn build_failed_status_event_always_uses_project_pg_stage() {
        let tid = SchemasTenantId::new();
        let ev = build_failed_status_event(&tid, "run-1", "err");
        assert_eq!(ev.stage, IngestStage::ProjectPg as i32);
    }

    #[test]
    fn project_pg_stage_seq_is_7() {
        assert_eq!(IngestStage::ProjectPg as i32, 7);
    }

    #[test]
    fn processing_status_value_is_2() {
        assert_eq!(IngestStatus::Processing as i32, 2);
    }
}
