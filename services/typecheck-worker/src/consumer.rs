//! Kafka consumer loop for `typecheck-worker`.
//!
//! Consumes `IngestRequest` from `rb.ingest.typecheck.commands`.
//! The envelope's `blob_ref` points to the clone tar.zst produced by `ingest-clone`.
//! For each command:
//!   1. Downloads the clone.tar.zst blob from rb-blob.
//!   2. Extracts to a temp directory.
//!   3. Walks `*.rs` files and parses each with syn to extract type signatures.
//!   4. Emits `TypecheckedItemEvent` per item to `rb.typechecked-items.v1`.
//!      — Files that fail syn parse emit no items and increment the error counter.
//!   5. Emits `IngestStatusEvent{stage:Typecheck, status:Done}` to `rb.projector.events`.

use std::sync::Arc;
use std::time::Duration;

use crate::helpers::{build_fqn, collect_rs_files, extract_tar_zst};
use crate::type_extractor::extract_typed_items;
use anyhow::{Context as _, Result};
use bytes::Bytes;
use metrics::counter;
use rb_blob::{BlobRef, BlobStore};
use rb_kafka::{EventEnvelope, Producer, RetryPolicy};
use rb_kafka_health::HealthyConsumer;
use rb_schemas::{
    IngestRequest, IngestStage, IngestStatus, IngestStatusEvent, TenantId, TypecheckedItemEvent,
    typechecked_item_event,
};
use sha2::{Digest as _, Sha256};

pub const TOPIC_TYPECHECK_COMMANDS: &str = "rb.ingest.typecheck.commands";
pub const TOPIC_TYPECHECKED_ITEMS: &str = "rb.typechecked-items.v1";
pub const TOPIC_PROJECTOR_EVENTS: &str = "rb.projector.events";

/// Inline threshold: items ≤ 512 KiB are embedded directly.
const INLINE_MAX_BYTES: usize = 512 * 1024;

struct TypecheckCtx {
    blob_store: Arc<dyn BlobStore>,
    item_producer: Arc<Producer<TypecheckedItemEvent>>,
    status_producer: Arc<Producer<IngestStatusEvent>>,
}

pub async fn run(
    mut consumer: HealthyConsumer<IngestRequest>,
    blob_store: Arc<dyn BlobStore>,
    item_producer: Arc<Producer<TypecheckedItemEvent>>,
    status_producer: Arc<Producer<IngestStatusEvent>>,
) {
    let ctx = Arc::new(TypecheckCtx {
        blob_store,
        item_producer,
        status_producer,
    });

    loop {
        match consumer.next().await {
            None => {
                tracing::info!("typecheck_worker: stream ended");
                break;
            }
            Some(Err(e)) => {
                tracing::error!("typecheck_worker: kafka error: {e}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Some(Ok(envelope)) => {
                let ingest_run_id = envelope.payload.ingest_run_id.clone();
                let event_id = envelope.payload.event_id.clone();
                let tenant_id = envelope.tenant_id;

                match process_typecheck(&ctx, &envelope).await {
                    Ok(()) => {
                        counter!("rb_typecheck_worker_total", "outcome" => "ok").increment(1);
                        if let Err(e) = consumer.commit(&envelope).await {
                            tracing::warn!(%ingest_run_id, "typecheck_worker: commit failed: {e}");
                        }
                    }
                    Err(e) => {
                        let attempt = envelope._meta.attempt + 1;
                        tracing::error!(
                            attempt,
                            %ingest_run_id,
                            tenant_id = %tenant_id,
                            "typecheck_worker: processing failed: {e:#}"
                        );
                        counter!("rb_typecheck_worker_total", "outcome" => "err").increment(1);
                        emit_failed_status(
                            &ctx.status_producer,
                            tenant_id,
                            &ingest_run_id,
                            &event_id,
                            &format!("typecheck_failed: {e:#}"),
                        )
                        .await;
                        let policy = RetryPolicy::default();
                        if policy.is_terminal(attempt) {
                            tracing::warn!(
                                attempt,
                                %ingest_run_id,
                                "typecheck_worker: max retries exceeded — routing to DLQ"
                            );
                            counter!("rb_typecheck_worker_dlq_total").increment(1);
                            if let Err(dlq_err) =
                                consumer.nack_to_dlq(&envelope, &format!("{e:#}")).await
                            {
                                tracing::error!(
                                    %ingest_run_id,
                                    "typecheck_worker: nack_to_dlq failed: {dlq_err}"
                                );
                            }
                            if let Err(ce) = consumer.commit(&envelope).await {
                                tracing::warn!(
                                    %ingest_run_id,
                                    "typecheck_worker: commit after DLQ failed: {ce}"
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

async fn process_typecheck(
    ctx: &TypecheckCtx,
    envelope: &EventEnvelope<IngestRequest>,
) -> Result<()> {
    let req = &envelope.payload;
    let tenant_id = envelope.tenant_id;
    let ingest_run_id = &req.ingest_run_id;

    emit_processing_status(&ctx.status_producer, tenant_id, req).await;

    let blob_uri = envelope
        .blob_ref
        .as_deref()
        .context("typecheck command missing blob_ref (clone tar.zst)")?;

    tracing::info!(%ingest_run_id, "typecheck_worker: downloading clone blob");

    let blob_ref =
        BlobRef::from_uri_minimal(blob_uri).context("invalid blob_ref URI in typecheck command")?;
    let archive_bytes = ctx
        .blob_store
        .get(&blob_ref)
        .await
        .context("failed to download clone blob")?;

    let tmp = tempfile::tempdir().context("failed to create temp dir")?;
    extract_tar_zst(&archive_bytes, tmp.path()).context("failed to extract clone tar.zst")?;

    tracing::info!(%ingest_run_id, "typecheck_worker: walking *.rs files");

    let rs_files = collect_rs_files(tmp.path());
    let file_count = rs_files.len();

    tracing::info!(%ingest_run_id, file_count, "typecheck_worker: extracting type signatures");

    let mut item_count = 0usize;
    let mut error_count = 0usize;

    for (rel_path, abs_path) in &rs_files {
        let source = match std::fs::read_to_string(abs_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    %ingest_run_id,
                    path = %rel_path,
                    "typecheck_worker: cannot read file: {e}"
                );
                error_count += 1;
                continue;
            }
        };

        let typed_items = extract_typed_items(&source);

        if typed_items.is_empty() {
            // Syn parse failed or no items — count as soft error, continue.
            if !source.trim().is_empty() {
                error_count += 1;
            }
            continue;
        }

        item_count += typed_items.len();

        for item in typed_items {
            let src_slice = item_source_slice(&source, item.line_start, item.line_end);
            emit_typechecked_item(ctx, tenant_id, req, rel_path, item, src_slice).await?;
        }
    }

    tracing::info!(
        %ingest_run_id,
        file_count,
        item_count,
        error_count,
        "typecheck_worker: typecheck done"
    );

    counter!("rb_typecheck_worker_items_total").increment(item_count as u64);
    counter!("rb_typecheck_worker_files_total").increment(file_count as u64);

    emit_done_status(ctx, tenant_id, req).await?;

    Ok(())
}

// ── Item source slicing ───────────────────────────────────────────────────────

/// Returns the source lines `[line_start, line_end]` (1-based, inclusive).
///
/// Avoids storing a full-file copy on every item: callers keep one `String`
/// per file and slice into it at emit time (`O(item_size)` instead of
/// `O(file_size)`).
fn item_source_slice(file_source: &str, line_start: u32, line_end: u32) -> &str {
    let start_0 = line_start.saturating_sub(1) as usize;
    let end_0 = line_end.saturating_sub(1) as usize;

    let mut current_line = 0usize;
    let mut byte_start: Option<usize> = if start_0 == 0 { Some(0) } else { None };

    for (i, ch) in file_source.char_indices() {
        if ch == '\n' {
            current_line += 1;
            if current_line == start_0 {
                byte_start = Some(i + 1);
            }
            if current_line > end_0 {
                return match byte_start {
                    Some(bs) => &file_source[bs..i],
                    None => file_source,
                };
            }
        }
    }

    match byte_start {
        Some(bs) => &file_source[bs..],
        None => file_source,
    }
}

// ── Kafka producers ───────────────────────────────────────────────────────────

async fn emit_typechecked_item(
    ctx: &TypecheckCtx,
    tenant_id: TenantId,
    req: &IngestRequest,
    source_path: &str,
    item: crate::type_extractor::TypedItemData,
    src_slice: &str,
) -> Result<()> {
    let sha256 = hex::encode(Sha256::digest(src_slice.as_bytes()));
    let src_bytes = src_slice.as_bytes().to_vec();

    let body = if src_bytes.len() <= INLINE_MAX_BYTES {
        typechecked_item_event::Body::InlinePayload(src_bytes)
    } else {
        let data = Bytes::from(src_bytes);
        #[allow(clippy::cast_possible_truncation)]
        let blob_ref = BlobRef::new(
            tenant_id.as_uuid(),
            &sha256,
            "text/x-rust",
            data.len() as u64,
        );
        ctx.blob_store
            .put(&blob_ref, data)
            .await
            .context("failed to store typechecked item source blob")?;
        typechecked_item_event::Body::BlobRef(blob_ref.to_uri())
    };

    let fqn = build_fqn(source_path, &item.name);

    let ev = TypecheckedItemEvent {
        ingest_run_id: req.ingest_run_id.clone(),
        tenant_id: tenant_id.to_string(),
        repo_id: req.repo_id.clone(),
        fqn,
        body: Some(body),
        resolved_type_signature: item.resolved_type_signature,
        trait_bounds: item.trait_bounds,
        emitted_at_ms: chrono::Utc::now().timestamp_millis(),
    };

    let envelope = rb_kafka::EventEnvelope::new(tenant_id, ev);
    let key = format!("{}.{}", req.tenant_id, req.repo_id);
    ctx.item_producer
        .publish(TOPIC_TYPECHECKED_ITEMS, key.as_bytes(), envelope)
        .await
        .context("failed to publish typechecked item")?;
    Ok(())
}

async fn emit_processing_status(
    producer: &Producer<IngestStatusEvent>,
    tenant_id: TenantId,
    req: &IngestRequest,
) {
    let ev = IngestStatusEvent {
        ingest_request_id: req.event_id.clone(),
        tenant_id: tenant_id.to_string(),
        status: IngestStatus::Processing as i32,
        error_message: String::new(),
        occurred_at_ms: chrono::Utc::now().timestamp_millis(),
        stage: IngestStage::Typecheck as i32,
        stage_seq: 4,
        ingest_run_id: req.ingest_run_id.clone(),
        attempt: 0,
    };
    let envelope = rb_kafka::EventEnvelope::new(tenant_id, ev);
    let key = tenant_id.to_string();
    if let Err(e) = producer
        .publish(TOPIC_PROJECTOR_EVENTS, key.as_bytes(), envelope)
        .await
    {
        tracing::warn!("typecheck_worker: failed to publish processing status: {e}");
    }
}

async fn emit_done_status(
    ctx: &TypecheckCtx,
    tenant_id: TenantId,
    req: &IngestRequest,
) -> Result<()> {
    let ev = IngestStatusEvent {
        ingest_request_id: req.event_id.clone(),
        tenant_id: tenant_id.to_string(),
        status: IngestStatus::Done as i32,
        error_message: String::new(),
        occurred_at_ms: chrono::Utc::now().timestamp_millis(),
        stage: IngestStage::Typecheck as i32,
        stage_seq: 4,
        ingest_run_id: req.ingest_run_id.clone(),
        attempt: 0,
    };
    let envelope = rb_kafka::EventEnvelope::new(tenant_id, ev);
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
        stage: IngestStage::Typecheck as i32,
        stage_seq: 4,
        ingest_run_id: ingest_run_id.to_owned(),
        attempt: 0,
    };
    let envelope = rb_kafka::EventEnvelope::new(tenant_id, ev);
    let key = tenant_id.to_string();
    if let Err(e) = producer
        .publish(TOPIC_PROJECTOR_EVENTS, key.as_bytes(), envelope)
        .await
    {
        tracing::error!("typecheck_worker: failed to publish failed status: {e}");
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::{build_fqn, collect_rs_files};

    #[test]
    fn topic_constants_are_distinct() {
        let topics = [
            TOPIC_TYPECHECK_COMMANDS,
            TOPIC_TYPECHECKED_ITEMS,
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
    fn inline_threshold_is_512kib() {
        assert_eq!(INLINE_MAX_BYTES, 512 * 1024);
    }

    #[test]
    fn build_fqn_combines_path_and_name() {
        assert_eq!(build_fqn("src/lib.rs", "MyStruct"), "src_lib::MyStruct");
    }

    #[test]
    fn collect_rs_files_finds_only_rs_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), b"").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"").unwrap();
        let files = collect_rs_files(dir.path());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "main.rs");
    }

    #[test]
    fn stage_seq_is_4_for_typecheck() {
        assert_eq!(IngestStage::Typecheck as i32, 4);
    }

    #[test]
    fn processing_status_value_is_2() {
        assert_eq!(IngestStatus::Processing as i32, 2);
    }

    #[test]
    fn retry_policy_terminal_at_max_attempts() {
        let policy = RetryPolicy::default();
        assert!(!policy.is_terminal(1));
        assert!(policy.is_terminal(3));
    }

    #[test]
    fn item_source_slice_single_line() {
        assert_eq!(item_source_slice("fn a() {}", 1, 1), "fn a() {}");
    }

    #[test]
    fn item_source_slice_first_of_two() {
        assert_eq!(item_source_slice("fn a() {}\nfn b() {}", 1, 1), "fn a() {}");
    }

    #[test]
    fn item_source_slice_second_of_two() {
        assert_eq!(item_source_slice("fn a() {}\nfn b() {}", 2, 2), "fn b() {}");
    }

    #[test]
    fn item_source_slice_multi_line() {
        let src = "fn a() {}\nfn b() {\n    x\n}\nfn c() {}";
        assert_eq!(item_source_slice(src, 2, 4), "fn b() {\n    x\n}");
    }

    #[test]
    fn item_source_slice_last_line_no_trailing_newline() {
        let src = "fn a() {}\nfn b() {}";
        assert_eq!(item_source_slice(src, 2, 2), "fn b() {}");
    }

    #[test]
    fn typechecked_item_event_fields_accessible() {
        let ev = TypecheckedItemEvent {
            ingest_run_id: "run-1".to_string(),
            tenant_id: "tenant-1".to_string(),
            repo_id: "repo-1".to_string(),
            fqn: "src_lib::MyFn".to_string(),
            body: Some(typechecked_item_event::Body::InlinePayload(
                b"fn my_fn() {}".to_vec(),
            )),
            resolved_type_signature: "fn my_fn()".to_string(),
            trait_bounds: vec!["T: Clone".to_string()],
            emitted_at_ms: 0,
        };
        assert_eq!(ev.fqn, "src_lib::MyFn");
        assert_eq!(ev.resolved_type_signature, "fn my_fn()");
        assert_eq!(ev.trait_bounds, vec!["T: Clone"]);
    }
}
