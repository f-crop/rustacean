//! Kafka consumer loop for `ingest-clone`.
//!
//! Consumes `IngestRequest` from `rb.ingest.clone.commands`.
//! For each command:
//!   1. Resolves the GitHub clone URL from Postgres + rb-github installation token.
//!   2. Clones the repo (--depth 50 --filter=blob:none) with 5-min inactivity timeout.
//!   3. Walks *.rs files, computes sha256 per file.
//!   4. Packages the clone as a tar.zst blob in rb-blob.
//!   5. Emits `SourceFileEvent` per file to `rb.source-files.v1`.
//!   6. Forwards an `IngestRequest` to `rb.ingest.expand.commands`.
//!   7. Emits `IngestStatusEvent{stage:Clone, status:Done}` to `rb.projector.events`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use bytes::Bytes;
use metrics::counter;
use rb_blob::{BlobRef, BlobStore};
use rb_github::GhApp;
use rb_kafka::{Consumer, EventEnvelope, Producer};
use rb_schemas::{
    IngestRequest, IngestStage, IngestStatus, IngestStatusEvent, SourceFileEvent, TenantId,
};
use sha2::{Digest as _, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

pub const TOPIC_CLONE_COMMANDS: &str = "rb.ingest.clone.commands";
pub const TOPIC_SOURCE_FILES: &str = "rb.source-files.v1";
pub const TOPIC_EXPAND_COMMANDS: &str = "rb.ingest.expand.commands";
pub const TOPIC_PROJECTOR_EVENTS: &str = "rb.projector.events";

const CONTENT_TYPE_TAR_ZST: &str = "application/x-tar+zstd";
const CONTENT_TYPE_RUST: &str = "text/x-rust";

/// Inline threshold: files ≤ 512 KiB are embedded directly; larger ones use `blob_ref`.
const INLINE_MAX_BYTES: usize = 512 * 1024;

/// Network inactivity timeout for `git clone`.
const CLONE_TIMEOUT_SECS: u64 = 300;

struct CloneCtx {
    pool: Arc<PgPool>,
    gh_app: Option<Arc<GhApp>>,
    blob_store: Arc<dyn BlobStore>,
    source_producer: Arc<Producer<SourceFileEvent>>,
    expand_producer: Arc<Producer<IngestRequest>>,
    status_producer: Arc<Producer<IngestStatusEvent>>,
}

pub async fn run(
    consumer: Consumer<IngestRequest>,
    pool: Arc<PgPool>,
    gh_app: Option<Arc<GhApp>>,
    blob_store: Arc<dyn BlobStore>,
    source_producer: Arc<Producer<SourceFileEvent>>,
    expand_producer: Arc<Producer<IngestRequest>>,
    status_producer: Arc<Producer<IngestStatusEvent>>,
) {
    let ctx = Arc::new(CloneCtx {
        pool,
        gh_app,
        blob_store,
        source_producer,
        expand_producer,
        status_producer,
    });

    loop {
        match consumer.next().await {
            None => {
                tracing::info!("ingest_clone: stream ended");
                break;
            }
            Some(Err(e)) => {
                tracing::error!("ingest_clone: kafka error: {e}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Some(Ok(envelope)) => {
                let ingest_run_id = envelope.payload.ingest_run_id.clone();
                let event_id = envelope.payload.event_id.clone();
                let tenant_id = envelope.tenant_id;
                match process_clone(&ctx, &envelope).await {
                    Ok(()) => {
                        counter!("rb_ingest_clone_total", "outcome" => "ok").increment(1);
                        if let Err(e) = consumer.commit(&envelope).await {
                            tracing::warn!(%ingest_run_id, "ingest_clone: commit failed: {e}");
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            %ingest_run_id,
                            tenant_id = %tenant_id,
                            "ingest_clone: processing failed: {e:#}"
                        );
                        counter!("rb_ingest_clone_total", "outcome" => "err").increment(1);
                        emit_failed_status(
                            &ctx.status_producer,
                            tenant_id,
                            &ingest_run_id,
                            &event_id,
                            &format!("clone_failed: {e:#}"),
                        )
                        .await;
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        }
    }
}

async fn process_clone(
    ctx: &CloneCtx,
    envelope: &EventEnvelope<IngestRequest>,
) -> Result<()> {
    let req = &envelope.payload;
    let tenant_id = envelope.tenant_id;
    let ingest_run_id = &req.ingest_run_id;

    let repo_id = Uuid::parse_str(&req.repo_id).context("invalid repo_id UUID")?;

    tracing::info!(%ingest_run_id, %repo_id, "ingest_clone: starting clone");

    // 1. Resolve the GitHub clone URL.
    let clone_url = resolve_clone_url(ctx, tenant_id, repo_id).await?;
    let redacted_url = redact_token(&clone_url);

    tracing::info!(%ingest_run_id, url = %redacted_url, "ingest_clone: cloning");

    // 2. Clone the repo into a temp directory.
    let tmp = tempfile::tempdir().context("failed to create temp dir")?;
    let clone_dir = tmp.path().join("repo");
    git_clone(&clone_url, &clone_dir).await?;

    tracing::info!(%ingest_run_id, "ingest_clone: clone done, walking .rs files");

    // 3. Walk *.rs files, compute sha256, collect metadata.
    let rs_files = collect_rs_files(&clone_dir)?;

    tracing::info!(%ingest_run_id, count = rs_files.len(), "ingest_clone: .rs files found");

    // 4. Package the clone directory as clone.tar.zst and store in rb-blob.
    let blob_uri = package_and_store_blob(ctx, tenant_id, &clone_dir).await?;

    tracing::info!(%ingest_run_id, %blob_uri, "ingest_clone: blob stored");

    // 5. Emit SourceFileEvent per .rs file.
    emit_source_files(ctx, tenant_id, req, &rs_files).await?;

    tracing::info!(%ingest_run_id, "ingest_clone: source file events emitted");

    // 6. Forward IngestRequest to rb.ingest.expand.commands.
    emit_expand_command(ctx, tenant_id, req, &blob_uri).await?;

    // 7. Emit IngestStatusEvent{stage: Clone, status: Done}.
    emit_done_status(&ctx.status_producer, tenant_id, req).await?;

    tracing::info!(%ingest_run_id, "ingest_clone: done");

    Ok(())
}

/// Resolves a GitHub HTTPS clone URL.
///
/// When GitHub App credentials are configured (`ctx.gh_app` is `Some`), embeds an
/// installation access token. Otherwise falls back to a `GITHUB_PAT` env var or,
/// if that is also absent, a plain unauthenticated URL (public repos only).
async fn resolve_clone_url(
    ctx: &CloneCtx,
    tenant_id: TenantId,
    repo_id: Uuid,
) -> Result<String> {
    // LEFT JOIN so the query succeeds even when no installation row exists (PAT/public path).
    let row: (String, Option<i64>) = sqlx::query_as(
        "SELECT r.full_name, gi.github_installation_id \
         FROM control.repos r \
         LEFT JOIN control.github_installations gi ON gi.id = r.installation_id \
         WHERE r.id = $1 AND r.tenant_id = $2 AND r.archived_at IS NULL",
    )
    .bind(repo_id)
    .bind(tenant_id.as_uuid())
    .fetch_one(ctx.pool.as_ref())
    .await
    .context("repo not found in database")?;

    let (full_name, installation_id) = row;

    if let Some(app) = &ctx.gh_app {
        let inst_id = installation_id
            .context("repo has no GitHub installation; cannot obtain clone token")?;
        let token = app
            .installation_token(inst_id)
            .await
            .context("failed to get GitHub installation token")?;
        // Embed the token as a URL credential so git authenticates without prompts.
        return Ok(format!(
            "https://x-access-token:{}@github.com/{}.git",
            token.expose(),
            full_name
        ));
    }

    // GH App not configured — try a personal access token, then fall back to public HTTPS.
    let pat = std::env::var("GITHUB_PAT").unwrap_or_default();
    if pat.is_empty() {
        Ok(format!("https://github.com/{}.git", full_name))
    } else {
        Ok(format!(
            "https://x-access-token:{}@github.com/{}.git",
            pat,
            full_name
        ))
    }
}

/// Removes the embedded token from a clone URL for safe logging.
fn redact_token(url: &str) -> String {
    if let Some(at_pos) = url.find('@') {
        if let Some(scheme_end) = url.find("://") {
            let scheme = &url[..scheme_end + 3];
            let rest = &url[at_pos..];
            return format!("{scheme}<token>{rest}");
        }
    }
    url.to_owned()
}

/// Runs `git clone --depth 50 --filter=blob:none <url> <dir>`.
///
/// Sets `GIT_HTTP_LOW_SPEED_LIMIT=1` / `GIT_HTTP_LOW_SPEED_TIME=300` so that
/// git itself aborts after 5 minutes of network inactivity. An outer tokio
/// timeout provides a hard upper bound.
async fn git_clone(url: &str, target: &Path) -> Result<()> {
    let status = tokio::time::timeout(
        Duration::from_secs(CLONE_TIMEOUT_SECS + 30),
        tokio::process::Command::new("git")
            .args([
                "clone",
                "--depth", "50",
                "--filter=blob:none",
                "--no-tags",
                "--single-branch",
                url,
                &target.to_string_lossy(),
            ])
            .env("GIT_TERMINAL_PROMPT", "0")
            // Abort if throughput drops below 1 byte/sec for 5 minutes.
            .env("GIT_HTTP_LOW_SPEED_LIMIT", "1")
            .env("GIT_HTTP_LOW_SPEED_TIME", CLONE_TIMEOUT_SECS.to_string())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status(),
    )
    .await
    .context("git clone timed out after 5 min inactivity")?
    .context("failed to spawn git")?;

    if !status.success() {
        anyhow::bail!(
            "git clone exited with code {}",
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}

struct RsFile {
    relative_path: String,
    sha256: String,
    size: u64,
    content: Vec<u8>,
}

/// Walk all *.rs files in `dir`, compute sha256, return metadata.
fn collect_rs_files(dir: &Path) -> Result<Vec<RsFile>> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("rs"))
    {
        let abs_path = entry.path();
        let content = std::fs::read(abs_path).with_context(|| {
            format!("failed to read {}", abs_path.display())
        })?;

        let sha256 = hex::encode(Sha256::digest(&content));
        let size = content.len() as u64;
        let relative_path = abs_path
            .strip_prefix(dir)
            .unwrap_or(abs_path)
            .to_string_lossy()
            .into_owned();

        files.push(RsFile {
            relative_path,
            sha256,
            size,
            content,
        });
    }
    Ok(files)
}

/// Creates a tar.zst of the clone directory, computes its sha256, stores in rb-blob.
/// Returns the blob URI.
async fn package_and_store_blob(
    ctx: &CloneCtx,
    tenant_id: TenantId,
    clone_dir: &Path,
) -> Result<String> {
    let clone_dir = clone_dir.to_owned();
    let archive_bytes = tokio::task::spawn_blocking(move || create_tar_zst(&clone_dir))
        .await
        .context("tar.zst task panicked")?
        .context("failed to create tar.zst")?;

    let sha256 = hex::encode(Sha256::digest(&archive_bytes));
    let size = archive_bytes.len() as u64;

    let blob_ref = BlobRef::new(
        tenant_id.as_uuid(),
        &sha256,
        CONTENT_TYPE_TAR_ZST,
        size,
    );
    let uri = blob_ref.to_uri();

    ctx.blob_store
        .put(&blob_ref, Bytes::from(archive_bytes))
        .await
        .context("failed to store clone blob")?;

    Ok(uri)
}

/// Creates an in-memory tar.zst archive of `dir`.
fn create_tar_zst(dir: &Path) -> Result<Vec<u8>> {

    let compressed = Vec::new();
    let mut encoder = zstd::Encoder::new(compressed, 3).context("zstd encoder init failed")?;

    {
        let mut builder = tar::Builder::new(&mut encoder);
        builder
            .append_dir_all(".", dir)
            .context("failed to append dir to tar")?;
        builder.finish().context("failed to finalize tar")?;
    }

    encoder.finish().context("failed to finish zstd stream")
}

/// Emits one `SourceFileEvent` per .rs file to `rb.source-files.v1`.
async fn emit_source_files(
    ctx: &CloneCtx,
    tenant_id: TenantId,
    req: &IngestRequest,
    files: &[RsFile],
) -> Result<()> {
    let now_ms = chrono::Utc::now().timestamp_millis();

    for file in files {
        let body = if file.content.len() <= INLINE_MAX_BYTES {
            Some(rb_schemas::source_file_event::Body::InlinePayload(
                file.content.clone(),
            ))
        } else {
            // Large files: store individually in blob store and use blob_ref.
            let file_sha = file.sha256.clone();
            let size = file.size;
            let content = file.content.clone();
            let blob_ref_obj = BlobRef::new(
                tenant_id.as_uuid(),
                &file_sha,
                CONTENT_TYPE_RUST,
                size,
            );
            let uri = blob_ref_obj.to_uri();
            ctx.blob_store
                .put(&blob_ref_obj, Bytes::from(content))
                .await
                .with_context(|| format!("failed to store file blob: {}", file.relative_path))?;
            Some(rb_schemas::source_file_event::Body::BlobRef(uri))
        };

        let ev = SourceFileEvent {
            ingest_run_id: req.ingest_run_id.clone(),
            tenant_id: tenant_id.to_string(),
            repo_id: req.repo_id.clone(),
            relative_path: file.relative_path.clone(),
            sha256: file.sha256.clone(),
            size_bytes: i64::try_from(file.size).unwrap_or(i64::MAX),
            emitted_at_ms: now_ms,
            body,
        };

        let envelope = rb_kafka::EventEnvelope::new(tenant_id, ev);
        let key = format!("{}.{}", req.ingest_run_id, file.relative_path);
        ctx.source_producer
            .publish(TOPIC_SOURCE_FILES, key.as_bytes(), envelope)
            .await
            .with_context(|| format!("failed to publish SourceFileEvent: {}", file.relative_path))?;
    }
    Ok(())
}

/// Forwards the `IngestRequest` to `rb.ingest.expand.commands` with the `blob_ref` attached.
async fn emit_expand_command(
    ctx: &CloneCtx,
    tenant_id: TenantId,
    req: &IngestRequest,
    blob_uri: &str,
) -> Result<()> {
    let expand_req = IngestRequest {
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

    let envelope = rb_kafka::EventEnvelope::new(tenant_id, expand_req)
        .with_blob_ref(blob_uri);

    let key = format!("{}.{}", req.tenant_id, req.repo_id);
    ctx.expand_producer
        .publish(TOPIC_EXPAND_COMMANDS, key.as_bytes(), envelope)
        .await
        .context("failed to publish expand command")?;

    Ok(())
}

async fn emit_done_status(
    producer: &Producer<IngestStatusEvent>,
    tenant_id: TenantId,
    req: &IngestRequest,
) -> Result<()> {
    let ev = IngestStatusEvent {
        ingest_request_id: req.event_id.clone(),
        tenant_id: tenant_id.to_string(),
        status: IngestStatus::Done as i32,
        error_message: String::new(),
        occurred_at_ms: chrono::Utc::now().timestamp_millis(),
        stage: IngestStage::Clone as i32,
        stage_seq: 1,
        ingest_run_id: req.ingest_run_id.clone(),
        attempt: 0,
    };
    let envelope = rb_kafka::EventEnvelope::new(tenant_id, ev);
    let key = tenant_id.to_string();
    producer
        .publish(TOPIC_PROJECTOR_EVENTS, key.as_bytes(), envelope)
        .await
        .context("failed to publish done status")?;
    Ok(())
}

async fn emit_failed_status(
    producer: &Producer<IngestStatusEvent>,
    tenant_id: TenantId,
    ingest_run_id: &str,
    event_id: &str,
    error_message: &str,
) {
    let ev = IngestStatusEvent {
        ingest_request_id: event_id.to_owned(),
        tenant_id: tenant_id.to_string(),
        status: IngestStatus::Failed as i32,
        error_message: error_message.to_owned(),
        occurred_at_ms: chrono::Utc::now().timestamp_millis(),
        stage: IngestStage::Clone as i32,
        stage_seq: 1,
        ingest_run_id: ingest_run_id.to_owned(),
        attempt: 0,
    };
    let envelope = rb_kafka::EventEnvelope::new(tenant_id, ev);
    let key = tenant_id.to_string();
    if let Err(e) = producer
        .publish(TOPIC_PROJECTOR_EVENTS, key.as_bytes(), envelope)
        .await
    {
        tracing::error!("ingest_clone: failed to publish failed status: {e}");
    }
}

// ── Topic constant consistency note ─────────────────────────────────────────
// projector-pg currently has `TOPIC_SOURCE_FILE = "rb.ingest.clone.commands"`.
// That is a placeholder from early scaffolding. The authoritative source-file
// topic is `rb.source-files.v1` (emitted here). projector-pg will be updated
// in its own PR to consume from `rb.source-files.v1`.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_token_replaces_credential() {
        let url =
            "https://x-access-token:ghp_secret123@github.com/owner/repo.git";
        let redacted = redact_token(url);
        assert!(!redacted.contains("ghp_secret123"), "token must be redacted");
        assert!(redacted.contains("<token>"), "placeholder must be present");
        assert!(redacted.contains("github.com/owner/repo.git"));
    }

    #[test]
    fn redact_token_leaves_plain_url_unchanged() {
        let url = "https://github.com/owner/repo.git";
        let redacted = redact_token(url);
        assert_eq!(redacted, url);
    }

    #[test]
    fn collect_rs_files_finds_rust_files() {
        let dir = tempfile::tempdir().unwrap();
        let rs_path = dir.path().join("lib.rs");
        let txt_path = dir.path().join("readme.txt");
        std::fs::write(&rs_path, b"fn main() {}").unwrap();
        std::fs::write(&txt_path, b"not rust").unwrap();

        let files = collect_rs_files(dir.path()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "lib.rs");
    }

    #[test]
    fn collect_rs_files_computes_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let content = b"fn foo() {}";
        std::fs::write(dir.path().join("foo.rs"), content).unwrap();

        let files = collect_rs_files(dir.path()).unwrap();
        assert_eq!(files.len(), 1);

        let expected = hex::encode(Sha256::digest(content));
        assert_eq!(files[0].sha256, expected);
        assert_eq!(files[0].size, content.len() as u64);
    }

    #[test]
    fn collect_rs_files_walks_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), b"fn main() {}").unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), b"pub fn foo() {}").unwrap();

        let mut files = collect_rs_files(dir.path()).unwrap();
        files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f.relative_path.contains("main.rs")));
        assert!(files.iter().any(|f| f.relative_path.contains("lib.rs")));
    }

    #[test]
    fn create_tar_zst_produces_non_empty_archive() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.rs"), b"fn main() {}").unwrap();

        let bytes = create_tar_zst(dir.path()).unwrap();
        assert!(!bytes.is_empty(), "tar.zst must be non-empty");
        // Zstd magic bytes: 0xFD2FB528 in little-endian
        assert_eq!(&bytes[..4], &[0x28, 0xB5, 0x2F, 0xFD]);
    }

    #[test]
    fn create_tar_zst_is_decompressible() {
        let dir = tempfile::tempdir().unwrap();
        let content = b"pub struct Foo;";
        std::fs::write(dir.path().join("foo.rs"), content).unwrap();

        let compressed = create_tar_zst(dir.path()).unwrap();

        // Decompress and verify a tar archive is inside.
        let decoded = zstd::decode_all(std::io::Cursor::new(&compressed)).unwrap();
        assert!(!decoded.is_empty());
    }

    #[test]
    fn topic_constants_are_distinct() {
        let topics = [
            TOPIC_CLONE_COMMANDS,
            TOPIC_SOURCE_FILES,
            TOPIC_EXPAND_COMMANDS,
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
    fn clone_timeout_is_five_minutes() {
        assert_eq!(CLONE_TIMEOUT_SECS, 300);
    }
}
