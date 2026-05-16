//! Kafka consumer loop for `expand-worker`.
//!
//! Consumes `IngestRequest` from `rb.ingest.expand.commands`.
//! The envelope's `blob_ref` points to the clone tar.zst produced by `ingest-clone`.
//! For each command:
//!   1. Downloads the clone.tar.zst blob from rb-blob.
//!   2. Extracts to a temp directory (see `archive`).
//!   3. Discovers workspace members (see `workspace`).
//!   4. For each member, resolves features via `rb-feature-resolver`.
//!   5. Runs `cargo expand --package <name> --features <list>`.
//!   6. Emits `ExpandedFileEvent` per crate (partial=true when cargo expand exits non-zero
//!      but still produced stdout).
//!   7. Forwards `IngestRequest` to `rb.ingest.parse.commands`.
//!   8. Emits `IngestStatusEvent{stage:Expand, status:Done}` to `rb.projector.events`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::archive::extract_tar_zst;
use crate::workspace::{WorkspaceMember, discover_workspace_members, load_manifest};

use anyhow::{Context as _, Result};
use bytes::Bytes;
use metrics::counter;
use rb_blob::{BlobRef, BlobStore};
use rb_kafka::{EventEnvelope, Producer, RetryPolicy};
use rb_kafka_health::HealthyConsumer;
use rb_schemas::{
    ExpandedFileEvent, IngestRequest, IngestStage, IngestStatus, IngestStatusEvent, TenantId,
    expanded_file_event,
};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

pub const TOPIC_EXPAND_COMMANDS: &str = "rb.ingest.expand.commands";
pub const TOPIC_EXPANDED_FILES: &str = "rb.expanded-files.v1";
pub const TOPIC_PARSE_COMMANDS: &str = "rb.ingest.parse.commands";
pub const TOPIC_PROJECTOR_EVENTS: &str = "rb.projector.events";

const INLINE_MAX_BYTES: usize = 512 * 1024;

struct ExpandCtx {
    blob_store: Arc<dyn BlobStore>,
    expanded_producer: Arc<Producer<ExpandedFileEvent>>,
    parse_producer: Arc<Producer<IngestRequest>>,
    status_producer: Arc<Producer<IngestStatusEvent>>,
}

pub async fn run(
    mut consumer: HealthyConsumer<IngestRequest>,
    blob_store: Arc<dyn BlobStore>,
    expanded_producer: Arc<Producer<ExpandedFileEvent>>,
    parse_producer: Arc<Producer<IngestRequest>>,
    status_producer: Arc<Producer<IngestStatusEvent>>,
) {
    let ctx = Arc::new(ExpandCtx {
        blob_store,
        expanded_producer,
        parse_producer,
        status_producer,
    });

    loop {
        match consumer.next().await {
            None => {
                tracing::info!("expand_worker: stream ended");
                break;
            }
            Some(Err(e)) => {
                tracing::error!("expand_worker: kafka error: {e}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Some(Ok(envelope)) => {
                let ingest_run_id = envelope.payload.ingest_run_id.clone();
                let event_id = envelope.payload.event_id.clone();
                let tenant_id = envelope.tenant_id;
                match process_expand(&ctx, &envelope).await {
                    Ok(()) => {
                        counter!("rb_expand_worker_total", "outcome" => "ok").increment(1);
                        if let Err(e) = consumer.commit(&envelope).await {
                            tracing::warn!(%ingest_run_id, "expand_worker: commit failed: {e}");
                        }
                    }
                    Err(e) => {
                        let attempt = envelope._meta.attempt + 1;
                        tracing::error!(
                            attempt,
                            %ingest_run_id,
                            tenant_id = %tenant_id,
                            "expand_worker: processing failed: {e:#}"
                        );
                        counter!("rb_expand_worker_total", "outcome" => "err").increment(1);
                        emit_failed_status(
                            &ctx.status_producer,
                            tenant_id,
                            &ingest_run_id,
                            &event_id,
                            &format!("expand_failed: {e:#}"),
                        )
                        .await;
                        let policy = RetryPolicy::default();
                        if policy.is_terminal(attempt) {
                            tracing::warn!(
                                attempt,
                                %ingest_run_id,
                                "expand_worker: max retries exceeded — routing to DLQ"
                            );
                            counter!("rb_expand_worker_dlq_total").increment(1);
                            if let Err(dlq_err) =
                                consumer.nack_to_dlq(&envelope, &format!("{e:#}")).await
                            {
                                tracing::error!(%ingest_run_id, "expand_worker: nack_to_dlq failed: {dlq_err}");
                            }
                            if let Err(commit_err) = consumer.commit(&envelope).await {
                                tracing::warn!(%ingest_run_id, "expand_worker: commit after DLQ failed: {commit_err}");
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

async fn process_expand(ctx: &ExpandCtx, envelope: &EventEnvelope<IngestRequest>) -> Result<()> {
    let req = &envelope.payload;
    let tenant_id = envelope.tenant_id;
    let ingest_run_id = &req.ingest_run_id;

    let blob_uri = envelope
        .blob_ref
        .as_deref()
        .context("expand command missing blob_ref (clone tar.zst)")?;

    tracing::info!(%ingest_run_id, "expand_worker: downloading clone blob");

    let blob_ref =
        BlobRef::from_uri_minimal(blob_uri).context("invalid blob_ref URI in expand command")?;
    let archive_bytes = ctx
        .blob_store
        .get(&blob_ref)
        .await
        .context("failed to download clone blob")?;

    let tmp = tempfile::tempdir().context("failed to create temp dir")?;
    let clone_dir = tmp.path().join("repo");
    std::fs::create_dir_all(&clone_dir).context("failed to create clone dir")?;

    extract_tar_zst(&archive_bytes, &clone_dir).context("failed to extract clone tar.zst")?;

    tracing::info!(%ingest_run_id, "expand_worker: running cargo expand");

    let members = discover_workspace_members(&clone_dir)?;
    tracing::info!(%ingest_run_id, count = members.len(), "expand_worker: workspace members found");

    let mut expanded_count = 0usize;
    let mut partial_count = 0usize;

    for member in &members {
        match expand_crate(ctx, tenant_id, req, &clone_dir, member).await {
            Ok(partial) => {
                expanded_count += 1;
                if partial {
                    partial_count += 1;
                }
            }
            Err(e) => {
                tracing::warn!(
                    %ingest_run_id,
                    crate_name = %member.name,
                    "expand_worker: crate expand failed (skipping): {e:#}"
                );
                counter!("rb_expand_worker_crate_skip_total").increment(1);
            }
        }
    }

    tracing::info!(
        %ingest_run_id,
        expanded_count,
        partial_count,
        "expand_worker: expand done"
    );

    // Forward to parse stage.
    emit_parse_command(ctx, tenant_id, req, blob_uri).await?;

    emit_done_status(ctx, tenant_id, req).await?;

    Ok(())
}

/// Expands one workspace member crate. Returns `true` if the event was partial.
async fn expand_crate(
    ctx: &ExpandCtx,
    tenant_id: TenantId,
    req: &IngestRequest,
    workspace_root: &Path,
    member: &WorkspaceMember,
) -> Result<bool> {
    let manifest_path = workspace_root.join(&member.manifest_rel_path);
    let manifest_dir = manifest_path
        .parent()
        .context("manifest path has no parent")?;

    let manifest = load_manifest(&manifest_path)?;
    let requested = rb_feature_resolver::FeatureSet::default();
    let resolved = rb_feature_resolver::resolve(workspace_root, &manifest, &requested)
        .context("feature resolution failed")?;

    let feature_list: Vec<String> = resolved.features().iter().cloned().collect();
    let features_str = feature_list.join(",");

    let mut cmd = std::process::Command::new("cargo");
    cmd.arg("expand").arg("--package").arg(&member.name);
    if !features_str.is_empty() {
        cmd.arg("--features").arg(&features_str);
    }
    cmd.current_dir(manifest_dir);

    let output = tokio::task::spawn_blocking(move || cmd.output())
        .await
        .context("spawn_blocking failed")??;

    let stdout = output.stdout;
    let partial = !output.status.success();

    if partial && stdout.is_empty() {
        anyhow::bail!(
            "cargo expand failed with no output: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    if partial {
        tracing::warn!(
            ingest_run_id = %req.ingest_run_id,
            crate_name = %member.name,
            stderr = %String::from_utf8_lossy(&output.stderr),
            "expand_worker: cargo expand exited non-zero — emitting partial event"
        );
    }

    let expanded_bytes = Bytes::from(stdout);
    #[allow(clippy::cast_possible_wrap)]
    let size_bytes = expanded_bytes.len() as i64;
    let sha256 = hex::encode(Sha256::digest(&expanded_bytes));

    let relative_path = member
        .manifest_rel_path
        .strip_suffix("/Cargo.toml")
        .unwrap_or(&member.manifest_rel_path)
        .to_string();
    let relative_path = if relative_path.is_empty() {
        "src/lib.rs".to_string()
    } else {
        format!("{relative_path}/src/lib.rs")
    };

    let source_sha256 = std::fs::read(workspace_root.join(&relative_path))
        .map(|bytes| hex::encode(Sha256::digest(&bytes)))
        .unwrap_or_default();

    let body = if expanded_bytes.len() <= INLINE_MAX_BYTES {
        expanded_file_event::Body::InlinePayload(expanded_bytes.to_vec())
    } else {
        let blob_ref = store_expanded_blob(ctx, tenant_id, &sha256, expanded_bytes).await?;
        expanded_file_event::Body::BlobRef(blob_ref)
    };

    let ev = ExpandedFileEvent {
        ingest_run_id: req.ingest_run_id.clone(),
        tenant_id: tenant_id.to_string(),
        repo_id: req.repo_id.clone(),
        relative_path,
        sha256,
        body: Some(body),
        size_bytes,
        emitted_at_ms: chrono::Utc::now().timestamp_millis(),
        source_sha256,
        features: feature_list,
        partial,
    };

    let envelope = rb_kafka::EventEnvelope::new(tenant_id, ev);
    let key = format!("{}.{}", req.tenant_id, req.repo_id);
    ctx.expanded_producer
        .publish(TOPIC_EXPANDED_FILES, key.as_bytes(), envelope)
        .await
        .context("failed to publish ExpandedFileEvent")?;

    Ok(partial)
}

async fn store_expanded_blob(
    ctx: &ExpandCtx,
    tenant_id: TenantId,
    sha256: &str,
    data: Bytes,
) -> Result<String> {
    let blob_ref = BlobRef::new(
        tenant_id.as_uuid(),
        sha256,
        "text/x-rust-expanded",
        data.len() as u64,
    );
    ctx.blob_store
        .put(&blob_ref, data)
        .await
        .context("failed to store expanded blob")?;
    Ok(blob_ref.to_uri())
}

async fn emit_parse_command(
    ctx: &ExpandCtx,
    tenant_id: TenantId,
    req: &IngestRequest,
    blob_uri: &str,
) -> Result<()> {
    let parse_req = IngestRequest {
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

    let envelope = rb_kafka::EventEnvelope::new(tenant_id, parse_req).with_blob_ref(blob_uri);

    let key = format!("{}.{}", req.tenant_id, req.repo_id);
    ctx.parse_producer
        .publish(TOPIC_PARSE_COMMANDS, key.as_bytes(), envelope)
        .await
        .context("failed to publish parse command")?;

    Ok(())
}

async fn emit_done_status(ctx: &ExpandCtx, tenant_id: TenantId, req: &IngestRequest) -> Result<()> {
    let ev = IngestStatusEvent {
        ingest_request_id: req.event_id.clone(),
        tenant_id: tenant_id.to_string(),
        status: IngestStatus::Done as i32,
        error_message: String::new(),
        occurred_at_ms: chrono::Utc::now().timestamp_millis(),
        stage: IngestStage::Expand as i32,
        stage_seq: 2,
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
        stage: IngestStage::Expand as i32,
        stage_seq: 2,
        ingest_run_id: ingest_run_id.to_owned(),
        attempt: 0,
    };
    let envelope = rb_kafka::EventEnvelope::new(tenant_id, ev);
    let key = tenant_id.to_string();
    if let Err(e) = producer
        .publish(TOPIC_PROJECTOR_EVENTS, key.as_bytes(), envelope)
        .await
    {
        tracing::error!("expand_worker: failed to publish failed status: {e}");
    }
}

// ── Topic constant tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::extract_tar_zst;
    use crate::workspace::discover_workspace_members;

    #[test]
    fn topic_constants_are_distinct() {
        let topics = [
            TOPIC_EXPAND_COMMANDS,
            TOPIC_EXPANDED_FILES,
            TOPIC_PARSE_COMMANDS,
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
    fn extract_tar_zst_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let content = b"fn main() {}";
        std::fs::write(dir.path().join("main.rs"), content).unwrap();

        let compressed = create_tar_zst_for_test(dir.path());
        let dest = tempfile::tempdir().unwrap();
        extract_tar_zst(&compressed, dest.path()).unwrap();

        let extracted = std::fs::read(dest.path().join("main.rs")).unwrap();
        assert_eq!(extracted, content);
    }

    #[test]
    fn discover_workspace_members_single_crate() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            b"[package]\nname = \"my-crate\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();

        let members = discover_workspace_members(dir.path()).unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].name, "my-crate");
    }

    #[test]
    fn discover_workspace_members_workspace_crate() {
        let dir = tempfile::tempdir().unwrap();
        // Root workspace Cargo.toml
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/alpha\"]\n",
        )
        .unwrap();
        // Member
        std::fs::create_dir_all(dir.path().join("crates/alpha")).unwrap();
        std::fs::write(
            dir.path().join("crates/alpha/Cargo.toml"),
            "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();

        let members = discover_workspace_members(dir.path()).unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].name, "alpha");
        assert!(members[0].manifest_rel_path.ends_with("Cargo.toml"));
    }

    #[test]
    fn stage_seq_is_2_for_expand() {
        // Expand is the second pipeline stage (Clone=1, Expand=2).
        assert_eq!(IngestStage::Expand as i32, 2);
    }

    #[test]
    fn retry_policy_terminal_at_max_attempts() {
        let policy = RetryPolicy::default();
        assert!(!policy.is_terminal(0));
        assert!(!policy.is_terminal(1));
        assert!(!policy.is_terminal(2));
        assert!(
            policy.is_terminal(3),
            "attempt 3 must be terminal (DLQ path)"
        );
    }

    #[test]
    fn retry_policy_provides_backoff_delays() {
        let policy = RetryPolicy::default();
        assert!(policy.next_delay(1).is_some());
        assert!(policy.next_delay(2).is_some());
        assert!(
            policy.next_delay(3).is_none(),
            "no delay after terminal attempt"
        );
    }

    #[test]
    fn source_sha256_computed_from_source_file() {
        let dir = tempfile::tempdir().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let content = b"pub fn foo() {}";
        std::fs::write(src_dir.join("lib.rs"), content).unwrap();

        let relative_path = "src/lib.rs";
        let computed = std::fs::read(dir.path().join(relative_path))
            .map(|bytes| hex::encode(sha2::Sha256::digest(&bytes)))
            .unwrap_or_default();

        let expected = hex::encode(sha2::Sha256::digest(content));
        assert_eq!(computed, expected);
        assert!(!computed.is_empty());
    }

    #[test]
    fn source_sha256_empty_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let result = std::fs::read(dir.path().join("src/lib.rs"))
            .map(|bytes| hex::encode(sha2::Sha256::digest(&bytes)))
            .unwrap_or_default();
        assert!(
            result.is_empty(),
            "missing source file should produce empty sha256"
        );
    }

    fn create_tar_zst_for_test(src: &Path) -> Vec<u8> {
        use std::io::Write as _;
        let mut archive_data = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut archive_data);
            for entry in walkdir::WalkDir::new(src).min_depth(1) {
                let entry = entry.unwrap();
                if entry.file_type().is_file() {
                    let rel = entry.path().strip_prefix(src).unwrap();
                    builder.append_path_with_name(entry.path(), rel).unwrap();
                }
            }
            builder.finish().unwrap();
        }
        let mut encoder = zstd::Encoder::new(Vec::new(), 3).unwrap();
        encoder.write_all(&archive_data).unwrap();
        encoder.finish().unwrap()
    }
}
