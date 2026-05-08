//! Kafka consumer loop for `embed-worker`.
//!
//! Consumes `TypecheckedItemEvent` from `rb.ingest.embed.commands`.
//! For each event:
//!   1. Resolves the item body (inline bytes or blob download).
//!   2. Builds the §3.5.7 composite embedding input.
//!   3. Calls Ollama to produce a float vector.
//!   4. Upserts the vector to Qdrant `rb_embeddings` (point id = sha256(tenant:repo:fqn)).
//!   5. Emits `EmbeddingPendingEvent` to `rb.projector.events`.
//!   6. Emits `IngestStatusEvent{stage:Embed, status:Done}` to `rb.projector.events`.
//!   7. Commits the Kafka offset.
//!
//! On transient errors the consumer sleeps and redelivers.  Persistent failures
//! after max retries go to the DLQ via `consumer.nack_to_dlq`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use metrics::counter;
use rb_blob::{BlobRef, BlobStore};
use rb_kafka::{Consumer, EventEnvelope, Producer, RetryPolicy};
use rb_schemas::{
    IngestStage, IngestStatus, IngestStatusEvent, TenantId, TypecheckedItemEvent,
    typechecked_item_event,
};
use uuid::Uuid;

use crate::embedder::build_composite;
use crate::qdrant::upsert_vector;

pub const TOPIC_EMBED_COMMANDS: &str = "rb.ingest.embed.commands";
pub const TOPIC_PROJECTOR_EVENTS: &str = "rb.projector.events";

struct EmbedCtx {
    blob_store: Arc<dyn BlobStore>,
    status_producer: Arc<Producer<IngestStatusEvent>>,
    ollama_url: String,
    embedding_model: String,
    embedding_dimensions: u32,
    qdrant_url: String,
    http: reqwest::Client,
}

pub async fn run(
    consumer: Consumer<TypecheckedItemEvent>,
    blob_store: Arc<dyn BlobStore>,
    status_producer: Arc<Producer<IngestStatusEvent>>,
    ollama_url: String,
    embedding_model: String,
    embedding_dimensions: u32,
    qdrant_url: String,
) {
    let ctx = Arc::new(EmbedCtx {
        blob_store,
        status_producer,
        ollama_url,
        embedding_model,
        embedding_dimensions,
        qdrant_url,
        http: reqwest::Client::new(),
    });

    loop {
        match consumer.next().await {
            None => {
                tracing::info!("embed_worker: stream ended");
                break;
            }
            Some(Err(e)) => {
                tracing::error!("embed_worker: kafka error: {e}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Some(Ok(envelope)) => {
                let ingest_run_id = envelope.payload.ingest_run_id.clone();
                let tenant_id = envelope.tenant_id;

                match process_item(&ctx, &envelope).await {
                    Ok(()) => {
                        counter!("rb_embed_worker_total", "outcome" => "ok").increment(1);
                        if let Err(e) = consumer.commit(&envelope).await {
                            tracing::warn!(%ingest_run_id, "embed_worker: commit failed: {e}");
                        }
                    }
                    Err(e) => {
                        let attempt = envelope._meta.attempt + 1;
                        tracing::error!(
                            attempt,
                            %ingest_run_id,
                            tenant_id = %tenant_id,
                            "embed_worker: processing failed: {e:#}"
                        );
                        counter!("rb_embed_worker_total", "outcome" => "err").increment(1);
                        emit_failed_status(
                            &ctx.status_producer,
                            tenant_id,
                            &ingest_run_id,
                            &Uuid::new_v4().to_string(),
                            &format!("embed_failed: {e:#}"),
                        )
                        .await;
                        let policy = RetryPolicy::default();
                        if policy.is_terminal(attempt) {
                            tracing::warn!(
                                attempt,
                                %ingest_run_id,
                                "embed_worker: max retries exceeded — routing to DLQ"
                            );
                            counter!("rb_embed_worker_dlq_total").increment(1);
                            if let Err(dlq_err) =
                                consumer.nack_to_dlq(&envelope, &format!("{e:#}")).await
                            {
                                tracing::error!(
                                    %ingest_run_id,
                                    "embed_worker: nack_to_dlq failed: {dlq_err}"
                                );
                            }
                            if let Err(ce) = consumer.commit(&envelope).await {
                                tracing::warn!(
                                    %ingest_run_id,
                                    "embed_worker: commit after DLQ failed: {ce}"
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
    ctx: &EmbedCtx,
    envelope: &EventEnvelope<TypecheckedItemEvent>,
) -> Result<()> {
    let ev = &envelope.payload;
    let tenant_id = envelope.tenant_id;
    let ingest_run_id = &ev.ingest_run_id;

    tracing::debug!(%ingest_run_id, fqn = %ev.fqn, "embed_worker: processing item");

    let source_text = resolve_body(ctx, ev.body.as_ref()).await?;

    let composite = build_composite(
        &ev.fqn,
        &ev.resolved_type_signature,
        &ev.trait_bounds,
        source_text.as_deref(),
    );

    let vector =
        crate::embedder::call_ollama(&ctx.http, &ctx.ollama_url, &ctx.embedding_model, &composite)
            .await
            .context("Ollama embedding call failed")?;

    upsert_vector(
        &ctx.http,
        &ctx.qdrant_url,
        tenant_id,
        &ev.repo_id,
        &ev.fqn,
        &ev.ingest_run_id,
        &ctx.embedding_model,
        ctx.embedding_dimensions,
        vector,
    )
    .await
    .context("Qdrant upsert failed")?;

    counter!("rb_embed_worker_vectors_total").increment(1);
    tracing::debug!(%ingest_run_id, fqn = %ev.fqn, "embed_worker: vector upserted");

    emit_qdrant_done_status(ctx, tenant_id, ev).await?;
    emit_done_status(ctx, tenant_id, ev).await?;

    Ok(())
}

// ── Body resolution ───────────────────────────────────────────────────────────

async fn resolve_body(
    ctx: &EmbedCtx,
    body: Option<&typechecked_item_event::Body>,
) -> Result<Option<String>> {
    match body {
        None => Ok(None),
        Some(typechecked_item_event::Body::InlinePayload(bytes)) => {
            let text =
                String::from_utf8(bytes.clone()).context("inline item body is not valid UTF-8")?;
            Ok(Some(text))
        }
        Some(typechecked_item_event::Body::BlobRef(uri)) => {
            let blob_ref = BlobRef::from_uri_minimal(uri)
                .context("invalid blob_ref URI in typechecked item")?;
            let data = ctx
                .blob_store
                .get(&blob_ref)
                .await
                .context("failed to download item body blob")?;
            let text =
                String::from_utf8(data.to_vec()).context("blob item body is not valid UTF-8")?;
            Ok(Some(text))
        }
    }
}

// ── Kafka producers ───────────────────────────────────────────────────────────

async fn emit_qdrant_done_status(
    ctx: &EmbedCtx,
    tenant_id: TenantId,
    ev: &TypecheckedItemEvent,
) -> Result<()> {
    let status_ev = IngestStatusEvent {
        ingest_request_id: Uuid::new_v4().to_string(),
        tenant_id: tenant_id.to_string(),
        status: IngestStatus::Done as i32,
        error_message: String::new(),
        occurred_at_ms: chrono::Utc::now().timestamp_millis(),
        stage: IngestStage::ProjectQdrant as i32,
        stage_seq: 9,
        ingest_run_id: ev.ingest_run_id.clone(),
        attempt: 0,
    };
    let envelope = rb_kafka::EventEnvelope::new(tenant_id, status_ev);
    let key = tenant_id.to_string();
    ctx.status_producer
        .publish(TOPIC_PROJECTOR_EVENTS, key.as_bytes(), envelope)
        .await
        .context("failed to publish qdrant done status")?;
    Ok(())
}

async fn emit_done_status(
    ctx: &EmbedCtx,
    tenant_id: TenantId,
    ev: &TypecheckedItemEvent,
) -> Result<()> {
    let status_ev = IngestStatusEvent {
        ingest_request_id: Uuid::new_v4().to_string(),
        tenant_id: tenant_id.to_string(),
        status: IngestStatus::Done as i32,
        error_message: String::new(),
        occurred_at_ms: chrono::Utc::now().timestamp_millis(),
        stage: IngestStage::Embed as i32,
        stage_seq: 6,
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
        stage: IngestStage::Embed as i32,
        stage_seq: 6,
        ingest_run_id: ingest_run_id.to_owned(),
        attempt: 0,
    };
    let envelope = rb_kafka::EventEnvelope::new(tenant_id, ev);
    let key = tenant_id.to_string();
    if let Err(e) = producer
        .publish(TOPIC_PROJECTOR_EVENTS, key.as_bytes(), envelope)
        .await
    {
        tracing::error!("embed_worker: failed to publish failed status: {e}");
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rb_schemas::IngestStage;

    #[test]
    fn topic_constants_are_distinct() {
        let topics = [TOPIC_EMBED_COMMANDS, TOPIC_PROJECTOR_EVENTS];
        let unique: std::collections::HashSet<_> = topics.iter().collect();
        assert_eq!(unique.len(), topics.len(), "topic constants must be unique");
    }

    #[test]
    fn stage_seq_is_6_for_embed() {
        assert_eq!(IngestStage::Embed as i32, 6);
    }

    #[test]
    fn retry_policy_terminal_at_max_attempts() {
        let policy = RetryPolicy::default();
        assert!(!policy.is_terminal(1));
        assert!(policy.is_terminal(3));
    }
}
