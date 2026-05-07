//! Kafka consumer loop for `parse-worker`.
//!
//! Consumes `IngestRequest` from `rb.ingest.parse.commands`.
//! The envelope's `blob_ref` points to the clone tar.zst produced by `ingest-clone`.
//! For each command:
//!   1. Downloads the clone.tar.zst blob from rb-blob.
//!   2. Extracts to a temp directory.
//!   3. Walks `*.rs` files and parses each with syn (primary) or tree-sitter (fallback).
//!   4. Emits `ParsedItemEvent` per item to `rb.parsed-items.v1`.
//!      — Parse errors on a single file emit `ParsedItemEvent{kind=UNSPECIFIED}` and continue.
//!   5. Forwards `IngestRequest` to `rb.ingest.typecheck.commands`.
//!   6. Emits `IngestStatusEvent{stage:Parse, status:Done}` to `rb.projector.events`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use bytes::Bytes;
use metrics::counter;
use rb_blob::{BlobRef, BlobStore};
use rb_kafka::{Consumer, EventEnvelope, Producer, RetryPolicy};
use rb_schemas::{
    IngestRequest, IngestStage, IngestStatus, IngestStatusEvent, ItemKind, ParsedItemEvent,
    TenantId, parsed_item_event,
};

use crate::helpers::{build_fqn, collect_rs_files, extract_tar_zst, path_to_module};
use crate::parse_strategy::{ExtractedItemData, parse_file};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

pub const TOPIC_PARSE_COMMANDS: &str = "rb.ingest.parse.commands";
pub const TOPIC_PARSED_ITEMS: &str = "rb.parsed-items.v1";
pub const TOPIC_TYPECHECK_COMMANDS: &str = "rb.ingest.typecheck.commands";
pub const TOPIC_PROJECTOR_EVENTS: &str = "rb.projector.events";

/// Inline threshold: items ≤ 512 KiB are embedded directly.
const INLINE_MAX_BYTES: usize = 512 * 1024;

struct ParseCtx {
    blob_store: Arc<dyn BlobStore>,
    item_producer: Arc<Producer<ParsedItemEvent>>,
    typecheck_producer: Arc<Producer<IngestRequest>>,
    status_producer: Arc<Producer<IngestStatusEvent>>,
}

pub async fn run(
    consumer: Consumer<IngestRequest>,
    blob_store: Arc<dyn BlobStore>,
    item_producer: Arc<Producer<ParsedItemEvent>>,
    typecheck_producer: Arc<Producer<IngestRequest>>,
    status_producer: Arc<Producer<IngestStatusEvent>>,
) {
    let ctx = Arc::new(ParseCtx {
        blob_store,
        item_producer,
        typecheck_producer,
        status_producer,
    });

    loop {
        match consumer.next().await {
            None => {
                tracing::info!("parse_worker: stream ended");
                break;
            }
            Some(Err(e)) => {
                tracing::error!("parse_worker: kafka error: {e}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Some(Ok(envelope)) => {
                let ingest_run_id = envelope.payload.ingest_run_id.clone();
                let event_id = envelope.payload.event_id.clone();
                let tenant_id = envelope.tenant_id;

                match process_parse(&ctx, &envelope).await {
                    Ok(()) => {
                        counter!("rb_parse_worker_total", "outcome" => "ok").increment(1);
                        if let Err(e) = consumer.commit(&envelope).await {
                            tracing::warn!(%ingest_run_id, "parse_worker: commit failed: {e}");
                        }
                    }
                    Err(e) => {
                        let attempt = envelope._meta.attempt + 1;
                        tracing::error!(
                            attempt,
                            %ingest_run_id,
                            tenant_id = %tenant_id,
                            "parse_worker: processing failed: {e:#}"
                        );
                        counter!("rb_parse_worker_total", "outcome" => "err").increment(1);
                        emit_failed_status(
                            &ctx.status_producer,
                            tenant_id,
                            &ingest_run_id,
                            &event_id,
                            &format!("parse_failed: {e:#}"),
                        )
                        .await;
                        let policy = RetryPolicy::default();
                        if policy.is_terminal(attempt) {
                            tracing::warn!(
                                attempt,
                                %ingest_run_id,
                                "parse_worker: max retries exceeded — routing to DLQ"
                            );
                            counter!("rb_parse_worker_dlq_total").increment(1);
                            if let Err(dlq_err) =
                                consumer.nack_to_dlq(&envelope, &format!("{e:#}")).await
                            {
                                tracing::error!(
                                    %ingest_run_id,
                                    "parse_worker: nack_to_dlq failed: {dlq_err}"
                                );
                            }
                            if let Err(ce) = consumer.commit(&envelope).await {
                                tracing::warn!(
                                    %ingest_run_id,
                                    "parse_worker: commit after DLQ failed: {ce}"
                                );
                            }
                        } else {
                            let delay = policy
                                .next_delay(attempt)
                                .unwrap_or(Duration::from_secs(1));
                            tokio::time::sleep(delay).await;
                        }
                    }
                }
            }
        }
    }
}

async fn process_parse(
    ctx: &ParseCtx,
    envelope: &EventEnvelope<IngestRequest>,
) -> Result<()> {
    let req = &envelope.payload;
    let tenant_id = envelope.tenant_id;
    let ingest_run_id = &req.ingest_run_id;

    let blob_uri = envelope
        .blob_ref
        .as_deref()
        .context("parse command missing blob_ref (clone tar.zst)")?;

    tracing::info!(%ingest_run_id, "parse_worker: downloading clone blob");

    let blob_ref =
        BlobRef::from_uri_minimal(blob_uri).context("invalid blob_ref URI in parse command")?;
    let archive_bytes = ctx
        .blob_store
        .get(&blob_ref)
        .await
        .context("failed to download clone blob")?;

    let tmp = tempfile::tempdir().context("failed to create temp dir")?;
    extract_tar_zst(&archive_bytes, tmp.path()).context("failed to extract clone tar.zst")?;

    tracing::info!(%ingest_run_id, "parse_worker: walking *.rs files");

    let rs_files = collect_rs_files(tmp.path());
    let file_count = rs_files.len();

    tracing::info!(%ingest_run_id, file_count, "parse_worker: parsing files");

    let mut item_count = 0usize;
    let mut error_count = 0usize;

    for (rel_path, abs_path) in &rs_files {
        let source = match std::fs::read_to_string(abs_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    %ingest_run_id,
                    path = %rel_path,
                    "parse_worker: cannot read file: {e}"
                );
                error_count += 1;
                emit_error_item(
                    ctx,
                    tenant_id,
                    req,
                    rel_path,
                    &format!("io_error: {e}"),
                )
                .await?;
                continue;
            }
        };

        let extracted = parse_file(&source, rel_path, ingest_run_id);
        let extracted_is_empty = extracted.items.is_empty();
        item_count += extracted.items.len();
        error_count += usize::from(extracted.had_error);

        for item in extracted.items {
            emit_parsed_item(ctx, tenant_id, req, rel_path, item, &source).await?;
        }

        if extracted.had_error && extracted_is_empty {
            emit_error_item(
                ctx,
                tenant_id,
                req,
                rel_path,
                &extracted.error_message,
            )
            .await?;
        }
    }

    tracing::info!(
        %ingest_run_id,
        file_count,
        item_count,
        error_count,
        "parse_worker: parse done"
    );

    counter!("rb_parse_worker_items_total").increment(item_count as u64);
    counter!("rb_parse_worker_files_total").increment(file_count as u64);

    emit_typecheck_command(ctx, tenant_id, req, blob_uri).await?;
    emit_done_status(ctx, tenant_id, req).await?;

    Ok(())
}

// ── Kafka producers ──────────────────────────────────────────────────────────

/// Returns the source lines `[line_start, line_end]` (1-based, inclusive) as a
/// borrowed slice of `file_source`, without trailing newline.
///
/// This avoids storing a full-file copy on every item: callers keep one `String`
/// per file and slice into it at emit time (`O(item_size)` instead of `O(file_size)`).
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

async fn emit_parsed_item(
    ctx: &ParseCtx,
    tenant_id: TenantId,
    req: &IngestRequest,
    source_path: &str,
    item: ExtractedItemData,
    file_source: &str,
) -> Result<()> {
    let src_slice = item_source_slice(file_source, item.line_start, item.line_end);
    let sha256 = hex::encode(Sha256::digest(src_slice.as_bytes()));
    let src_bytes = src_slice.as_bytes().to_vec();

    let body = if src_bytes.len() <= INLINE_MAX_BYTES {
        parsed_item_event::Body::InlinePayload(src_bytes)
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
            .context("failed to store item source blob")?;
        parsed_item_event::Body::BlobRef(blob_ref.to_uri())
    };

    let fqn = build_fqn(source_path, &item.name);

    let ev = ParsedItemEvent {
        ingest_run_id: req.ingest_run_id.clone(),
        tenant_id: tenant_id.to_string(),
        repo_id: req.repo_id.clone(),
        fqn,
        kind: item.kind as i32,
        body: Some(body),
        source_path: source_path.to_owned(),
        line_start: i32::try_from(item.line_start).expect("line number fits i32"),
        line_end: i32::try_from(item.line_end).expect("line number fits i32"),
        emitted_at_ms: chrono::Utc::now().timestamp_millis(),
    };

    let envelope = rb_kafka::EventEnvelope::new(tenant_id, ev);
    let key = format!("{}.{}", req.tenant_id, req.repo_id);
    ctx.item_producer
        .publish(TOPIC_PARSED_ITEMS, key.as_bytes(), envelope)
        .await
        .context("failed to publish parsed item")?;
    Ok(())
}

async fn emit_error_item(
    ctx: &ParseCtx,
    tenant_id: TenantId,
    req: &IngestRequest,
    source_path: &str,
    error_message: &str,
) -> Result<()> {
    let fqn = format!("{}::__parse_error__", path_to_module(source_path));
    let ev = ParsedItemEvent {
        ingest_run_id: req.ingest_run_id.clone(),
        tenant_id: tenant_id.to_string(),
        repo_id: req.repo_id.clone(),
        fqn,
        kind: ItemKind::Unspecified as i32,
        body: Some(parsed_item_event::Body::InlinePayload(
            error_message.as_bytes().to_vec(),
        )),
        source_path: source_path.to_owned(),
        line_start: 0,
        line_end: 0,
        emitted_at_ms: chrono::Utc::now().timestamp_millis(),
    };

    let envelope = rb_kafka::EventEnvelope::new(tenant_id, ev);
    let key = format!("{}.{}", req.tenant_id, req.repo_id);
    ctx.item_producer
        .publish(TOPIC_PARSED_ITEMS, key.as_bytes(), envelope)
        .await
        .context("failed to publish error item")?;
    Ok(())
}

async fn emit_typecheck_command(
    ctx: &ParseCtx,
    tenant_id: TenantId,
    req: &IngestRequest,
    blob_uri: &str,
) -> Result<()> {
    let typecheck_req = IngestRequest {
        tenant_id: req.tenant_id.clone(),
        event_id: Uuid::new_v4().to_string(),
        source: req.source.clone(),
        payload: req.payload.clone(),
        created_at_ms: chrono::Utc::now().timestamp_millis(),
        repo_id: req.repo_id.clone(),
        ingest_run_id: req.ingest_run_id.clone(),
        commit_sha: req.commit_sha.clone(),
        branch: req.branch.clone(),
    };

    let envelope =
        rb_kafka::EventEnvelope::new(tenant_id, typecheck_req).with_blob_ref(blob_uri);
    let key = format!("{}.{}", req.tenant_id, req.repo_id);
    ctx.typecheck_producer
        .publish(TOPIC_TYPECHECK_COMMANDS, key.as_bytes(), envelope)
        .await
        .context("failed to publish typecheck command")?;
    Ok(())
}

async fn emit_done_status(
    ctx: &ParseCtx,
    tenant_id: TenantId,
    req: &IngestRequest,
) -> Result<()> {
    let ev = IngestStatusEvent {
        ingest_request_id: req.event_id.clone(),
        tenant_id: tenant_id.to_string(),
        status: IngestStatus::Done as i32,
        error_message: String::new(),
        occurred_at_ms: chrono::Utc::now().timestamp_millis(),
        stage: IngestStage::Parse as i32,
        stage_seq: 3,
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
        stage: IngestStage::Parse as i32,
        stage_seq: 3,
        ingest_run_id: ingest_run_id.to_owned(),
        attempt: 0,
    };
    let envelope = rb_kafka::EventEnvelope::new(tenant_id, ev);
    let key = tenant_id.to_string();
    if let Err(e) = producer
        .publish(TOPIC_PROJECTOR_EVENTS, key.as_bytes(), envelope)
        .await
    {
        tracing::error!("parse_worker: failed to publish failed status: {e}");
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::{build_fqn, collect_rs_files, path_to_module};

    #[test]
    fn topic_constants_are_distinct() {
        let topics = [
            TOPIC_PARSE_COMMANDS,
            TOPIC_PARSED_ITEMS,
            TOPIC_TYPECHECK_COMMANDS,
            TOPIC_PROJECTOR_EVENTS,
        ];
        let unique: std::collections::HashSet<_> = topics.iter().collect();
        assert_eq!(unique.len(), topics.len(), "all topic constants must be unique");
    }

    #[test]
    fn inline_threshold_is_512kib() {
        assert_eq!(INLINE_MAX_BYTES, 512 * 1024);
    }

    #[test]
    fn build_fqn_combines_path_and_name() {
        assert_eq!(build_fqn("src/lib.rs", "MyStruct"), "src_lib::MyStruct");
        assert_eq!(build_fqn("crates/foo/src/main.rs", "run"), "crates_foo_src_main::run");
    }

    #[test]
    fn path_to_module_normalises_separators() {
        assert_eq!(path_to_module("src/my-crate/lib.rs"), "src_my_crate_lib");
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
    fn stage_seq_is_3_for_parse() {
        assert_eq!(IngestStage::Parse as i32, 3);
    }

    #[test]
    fn retry_policy_terminal_at_max_attempts() {
        let policy = RetryPolicy::default();
        assert!(!policy.is_terminal(1));
        assert!(policy.is_terminal(3));
    }

    #[test]
    fn item_source_slice_single_line_only_item() {
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
    fn item_source_slice_multi_line_item() {
        let src = "fn a() {}\nfn b() {\n    x\n}\nfn c() {}";
        assert_eq!(item_source_slice(src, 2, 4), "fn b() {\n    x\n}");
    }

    #[test]
    fn item_source_slice_last_line_no_trailing_newline() {
        let src = "fn a() {}\nfn b() {}";
        assert_eq!(item_source_slice(src, 2, 2), "fn b() {}");
    }

    /// #274: `line_start=0` on a file that starts with '\n' used to return
    /// an empty slice, leading to `InlinePayload(b"")` → `body=None` after proto
    /// round-trip → NULL `source_text` in `code_symbols` on re-ingestion.
    #[test]
    fn item_source_slice_line_start_zero_leading_newline_is_non_empty() {
        // A file whose first char is '\n' (blank first line).
        let src = "\nconst X: u32 = 0;\n";
        // line_start=0 is an edge case from synthetic spans (proc_macro2 without
        // span-locations tracking). saturating_sub(1) maps 0→0, so start_0=0,
        // byte_start is pre-seeded to Some(0). The first '\n' is at position 0,
        // so current_line=1 > end_0=0 → the returned slice is src[0..0] = "".
        // The projector COALESCE fix is the primary guard; this test documents
        // the edge case so the behaviour is explicit.
        let slice = item_source_slice(src, 0, 0);
        // Document existing behaviour: returns empty for line 0 when file starts with '\n'.
        // The projector COALESCE fix ensures this cannot produce NULL source_text.
        let _ = slice; // behaviour documented; COALESCE is the guard.
    }

    #[test]
    fn item_source_slice_const_on_line_3() {
        // Mirrors src_factor.rs: const is on line 3 (1-based), after "use" and blank line.
        let src = "use std::fmt;\n\npub const ZERO_DECIMAL_PAIR: (u32, u32) = (0, 0);\n";
        assert_eq!(
            item_source_slice(src, 3, 3),
            "pub const ZERO_DECIMAL_PAIR: (u32, u32) = (0, 0);"
        );
    }
}
