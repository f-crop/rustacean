//! Kafka consumer loop for `ingest-graph`.
//!
//! Consumes `TypecheckedItemEvent` from `rb.typechecked-items.v1`.
//! For each event:
//!   1. Resolves the item body (inline bytes or blob download).
//!   2. Calls [`extract_relations`] to derive graph edges.
//!   3. Emits one `GraphRelationEvent` per relation to `rb.ingest.graph.commands`.
//!   4. Forwards the `TypecheckedItemEvent` to `rb.ingest.embed.commands` (Stage 6 cascade).
//!   5. Emits `IngestStatusEvent{stage:Extract, status:Done}` to `rb.projector.events`.
//!   6. Commits the consumer offset.
//!
//! On transient errors the consumer sleeps and redelivers (no DLQ for per-item
//! events; a single bad item is skipped after max retries via a DLQ nack).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use metrics::counter;
use rb_blob::{BlobRef, BlobStore};
use rb_kafka::{Consumer, EventEnvelope, Producer, RetryPolicy};
use rb_schemas::{
    GraphRelationEvent, IngestStage, IngestStatus, IngestStatusEvent, TenantId,
    TypecheckedItemEvent, typechecked_item_event,
};
use uuid::Uuid;

use crate::extractor::extract_relations;

pub const TOPIC_TYPECHECKED_ITEMS: &str = "rb.typechecked-items.v1";
pub const TOPIC_GRAPH_COMMANDS: &str = "rb.ingest.graph.commands";
pub const TOPIC_EMBED_COMMANDS: &str = "rb.ingest.embed.commands";
pub const TOPIC_PROJECTOR_EVENTS: &str = "rb.projector.events";

struct GraphCtx {
    blob_store: Arc<dyn BlobStore>,
    relation_producer: Arc<Producer<GraphRelationEvent>>,
    embed_producer: Arc<Producer<TypecheckedItemEvent>>,
    status_producer: Arc<Producer<IngestStatusEvent>>,
}

pub async fn run(
    consumer: Consumer<TypecheckedItemEvent>,
    blob_store: Arc<dyn BlobStore>,
    relation_producer: Arc<Producer<GraphRelationEvent>>,
    embed_producer: Arc<Producer<TypecheckedItemEvent>>,
    status_producer: Arc<Producer<IngestStatusEvent>>,
) {
    let ctx = Arc::new(GraphCtx {
        blob_store,
        relation_producer,
        embed_producer,
        status_producer,
    });

    loop {
        match consumer.next().await {
            None => {
                tracing::info!("ingest_graph: stream ended");
                break;
            }
            Some(Err(e)) => {
                tracing::error!("ingest_graph: kafka error: {e}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Some(Ok(envelope)) => {
                let ingest_run_id = envelope.payload.ingest_run_id.clone();
                let tenant_id = envelope.tenant_id;

                match process_item(&ctx, &envelope).await {
                    Ok(()) => {
                        counter!("rb_ingest_graph_total", "outcome" => "ok").increment(1);
                        if let Err(e) = consumer.commit(&envelope).await {
                            tracing::warn!(%ingest_run_id, "ingest_graph: commit failed: {e}");
                        }
                    }
                    Err(e) => {
                        let attempt = envelope._meta.attempt + 1;
                        tracing::error!(
                            attempt,
                            %ingest_run_id,
                            tenant_id = %tenant_id,
                            "ingest_graph: processing failed: {e:#}"
                        );
                        counter!("rb_ingest_graph_total", "outcome" => "err").increment(1);
                        emit_failed_status(
                            &ctx.status_producer,
                            tenant_id,
                            &ingest_run_id,
                            &envelope.payload.fqn,
                            &format!("extract_failed: {e:#}"),
                        )
                        .await;
                        let policy = RetryPolicy::default();
                        if policy.is_terminal(attempt) {
                            tracing::warn!(
                                attempt,
                                %ingest_run_id,
                                "ingest_graph: max retries exceeded — routing to DLQ"
                            );
                            counter!("rb_ingest_graph_dlq_total").increment(1);
                            if let Err(dlq_err) =
                                consumer.nack_to_dlq(&envelope, &format!("{e:#}")).await
                            {
                                tracing::error!(
                                    %ingest_run_id,
                                    "ingest_graph: nack_to_dlq failed: {dlq_err}"
                                );
                            }
                            if let Err(ce) = consumer.commit(&envelope).await {
                                tracing::warn!(
                                    %ingest_run_id,
                                    "ingest_graph: commit after DLQ failed: {ce}"
                                );
                            }
                        } else {
                            let delay =
                                policy.next_delay(attempt).unwrap_or(Duration::from_secs(1));
                            tokio::time::sleep(delay).await;
                        }
                    }
                }
            }
        }
    }
}

async fn process_item(
    ctx: &GraphCtx,
    envelope: &EventEnvelope<TypecheckedItemEvent>,
) -> Result<()> {
    let ev = &envelope.payload;
    let tenant_id = envelope.tenant_id;
    let ingest_run_id = &ev.ingest_run_id;

    tracing::debug!(%ingest_run_id, fqn = %ev.fqn, "ingest_graph: processing item");

    let body = resolve_body(ctx, ev.body.as_ref()).await?;

    let relations = extract_relations(
        &ev.fqn,
        &ev.resolved_type_signature,
        &ev.trait_bounds,
        &body,
    );

    let relation_count = relations.len();

    for rel in relations {
        emit_relation(ctx, tenant_id, ev, rel).await?;
    }

    counter!("rb_ingest_graph_relations_total").increment(relation_count as u64);
    tracing::debug!(
        %ingest_run_id,
        fqn = %ev.fqn,
        relation_count,
        "ingest_graph: extraction done"
    );

    // Cascade to Stage 6: forward the original item to embed-worker.
    emit_embed_command(ctx, tenant_id, ev).await?;

    emit_done_status(ctx, tenant_id, ev).await?;

    Ok(())
}

// ── Body resolution ───────────────────────────────────────────────────────────

async fn resolve_body(
    ctx: &GraphCtx,
    body: Option<&typechecked_item_event::Body>,
) -> Result<String> {
    match body {
        None => Ok(String::new()),
        Some(typechecked_item_event::Body::InlinePayload(bytes)) => {
            String::from_utf8(bytes.clone()).context("inline item body is not valid UTF-8")
        }
        Some(typechecked_item_event::Body::BlobRef(uri)) => {
            let blob_ref = BlobRef::from_uri_minimal(uri)
                .context("invalid blob_ref URI in typechecked item")?;
            let data = ctx
                .blob_store
                .get(&blob_ref)
                .await
                .context("failed to download item body blob")?;
            String::from_utf8(data.to_vec()).context("blob item body is not valid UTF-8")
        }
    }
}

// ── Kafka producers ───────────────────────────────────────────────────────────

async fn emit_relation(
    ctx: &GraphCtx,
    tenant_id: TenantId,
    ev: &TypecheckedItemEvent,
    rel: crate::extractor::Relation,
) -> Result<()> {
    let graph_ev = GraphRelationEvent {
        ingest_run_id: ev.ingest_run_id.clone(),
        tenant_id: tenant_id.to_string(),
        repo_id: ev.repo_id.clone(),
        from_fqn: rel.from_fqn,
        to_fqn: rel.to_fqn,
        kind: rel.kind as i32,
        emitted_at_ms: chrono::Utc::now().timestamp_millis(),
    };
    let envelope = rb_kafka::EventEnvelope::new(tenant_id, graph_ev);
    let key = format!("{}.{}", ev.tenant_id, ev.repo_id);
    ctx.relation_producer
        .publish(TOPIC_GRAPH_COMMANDS, key.as_bytes(), envelope)
        .await
        .context("failed to publish graph relation event")?;
    Ok(())
}

async fn emit_embed_command(
    ctx: &GraphCtx,
    tenant_id: TenantId,
    ev: &TypecheckedItemEvent,
) -> Result<()> {
    let envelope = rb_kafka::EventEnvelope::new(tenant_id, ev.clone());
    let key = format!("{}.{}", ev.tenant_id, ev.repo_id);
    ctx.embed_producer
        .publish(TOPIC_EMBED_COMMANDS, key.as_bytes(), envelope)
        .await
        .context("failed to forward item to embed commands")?;
    Ok(())
}

async fn emit_done_status(
    ctx: &GraphCtx,
    tenant_id: TenantId,
    ev: &TypecheckedItemEvent,
) -> Result<()> {
    let status_ev = IngestStatusEvent {
        ingest_request_id: Uuid::new_v4().to_string(),
        tenant_id: tenant_id.to_string(),
        status: IngestStatus::Done as i32,
        error_message: String::new(),
        occurred_at_ms: chrono::Utc::now().timestamp_millis(),
        stage: IngestStage::Extract as i32,
        stage_seq: 5,
        ingest_run_id: ev.ingest_run_id.clone(),
        attempt: 0,
    };
    let envelope = rb_kafka::EventEnvelope::new(tenant_id, status_ev);
    let key = tenant_id.to_string();
    ctx.status_producer
        .publish(TOPIC_PROJECTOR_EVENTS, key.as_bytes(), envelope)
        .await
        .context("failed to publish done status")?;
    Ok(())
}

async fn emit_failed_status(
    producer: &Producer<IngestStatusEvent>,
    tenant_id: TenantId,
    ingest_run_id: &str,
    ingest_request_id: &str,
    error_message: &str,
) {
    let ev = IngestStatusEvent {
        ingest_request_id: ingest_request_id.to_owned(),
        tenant_id: tenant_id.to_string(),
        status: IngestStatus::Failed as i32,
        error_message: error_message.to_owned(),
        occurred_at_ms: chrono::Utc::now().timestamp_millis(),
        stage: IngestStage::Extract as i32,
        stage_seq: 5,
        ingest_run_id: ingest_run_id.to_owned(),
        attempt: 0,
    };
    let envelope = rb_kafka::EventEnvelope::new(tenant_id, ev);
    let key = tenant_id.to_string();
    if let Err(e) = producer
        .publish(TOPIC_PROJECTOR_EVENTS, key.as_bytes(), envelope)
        .await
    {
        tracing::error!("ingest_graph: failed to publish failed status: {e}");
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rb_schemas::RelationKind;

    #[test]
    fn topic_constants_are_distinct() {
        let topics = [
            TOPIC_TYPECHECKED_ITEMS,
            TOPIC_GRAPH_COMMANDS,
            TOPIC_EMBED_COMMANDS,
            TOPIC_PROJECTOR_EVENTS,
        ];
        let unique: std::collections::HashSet<_> = topics.iter().collect();
        assert_eq!(
            unique.len(),
            topics.len(),
            "all topic constants must be unique"
        );
    }

    #[test]
    fn stage_seq_is_5_for_extract() {
        assert_eq!(IngestStage::Extract as i32, 5);
    }

    #[test]
    fn retry_policy_terminal_at_max_attempts() {
        let policy = RetryPolicy::default();
        assert!(!policy.is_terminal(1));
        assert!(policy.is_terminal(3));
    }

    #[test]
    fn graph_relation_event_fields_accessible() {
        let ev = GraphRelationEvent {
            ingest_run_id: "run-1".to_string(),
            tenant_id: "tenant-1".to_string(),
            repo_id: "repo-1".to_string(),
            from_fqn: "src_lib::Foo".to_string(),
            to_fqn: "Display".to_string(),
            kind: RelationKind::Impls as i32,
            emitted_at_ms: 0,
        };
        assert_eq!(ev.from_fqn, "src_lib::Foo");
        assert_eq!(ev.kind, RelationKind::Impls as i32);
    }
}
