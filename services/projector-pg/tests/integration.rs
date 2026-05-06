//! Integration tests for `projector-pg` projection logic.
//!
//! Requires a running Postgres instance. Set `TEST_DATABASE_URL` to run:
//!   docker compose -f compose/test.yml up -d postgres
//!   `TEST_DATABASE_URL=postgres://rustbrain:rustbrain@localhost:5433/rustbrain` \
//!     cargo test -p projector-pg

mod common;

use common::{make_pool, new_ctx, setup_tenant, teardown_tenant, test_url};
use rb_schemas::{
    GraphRelationEvent, ItemKind, ParsedItemEvent, RelationKind,
    SourceFileEvent, TenantId, source_file_event, parsed_item_event,
};

macro_rules! skip_no_db {
    ($url:ident) => {
        let Some($url) = test_url() else {
            eprintln!("SKIP: TEST_DATABASE_URL not set");
            return;
        };
    };
}

// ── source file projection ───────────────────────────────────────────────────

#[tokio::test]
async fn write_source_file_inserts_row() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx = new_ctx();
    setup_tenant(&pool, &ctx).await;

    let tid = *ctx.tenant_id();
    let repo_id = uuid::Uuid::new_v4();
    let ev = SourceFileEvent {
        ingest_run_id: "run-1".to_string(),
        tenant_id: tid.to_string(),
        repo_id: repo_id.to_string(),
        relative_path: "src/main.rs".to_string(),
        sha256: "abc123".to_string(),
        size_bytes: 1024,
        emitted_at_ms: 0,
        body: None,
    };

    projector_pg::write_source_file(&pool, &ctx, &tid, &ev)
        .await
        .expect("write_source_file");

    let count: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {}.code_files",
        ctx.schema_name()
    ))
    .fetch_one(pool.control())
    .await
    .unwrap();
    assert_eq!(count, 1, "one row must be inserted");

    teardown_tenant(&pool, &ctx).await;
}

#[tokio::test]
async fn write_source_file_upsert_is_idempotent() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx = new_ctx();
    setup_tenant(&pool, &ctx).await;

    let tid = *ctx.tenant_id();
    let repo_id = uuid::Uuid::new_v4();
    let ev = SourceFileEvent {
        ingest_run_id: "run-1".to_string(),
        tenant_id: tid.to_string(),
        repo_id: repo_id.to_string(),
        relative_path: "src/main.rs".to_string(),
        sha256: "v1".to_string(),
        size_bytes: 100,
        emitted_at_ms: 0,
        body: None,
    };

    projector_pg::write_source_file(&pool, &ctx, &tid, &ev)
        .await
        .expect("first write");

    let ev_v2 = SourceFileEvent {
        sha256: "v2".to_string(),
        size_bytes: 200,
        ..ev.clone()
    };
    projector_pg::write_source_file(&pool, &ctx, &tid, &ev_v2)
        .await
        .expect("second write (upsert)");

    let count: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {}.code_files",
        ctx.schema_name()
    ))
    .fetch_one(pool.control())
    .await
    .unwrap();
    assert_eq!(count, 1, "upsert must not duplicate rows");

    let sha: String = sqlx::query_scalar(&format!(
        "SELECT sha256 FROM {}.code_files WHERE repo_id = $1 AND relative_path = $2",
        ctx.schema_name()
    ))
    .bind(repo_id)
    .bind("src/main.rs")
    .fetch_one(pool.control())
    .await
    .unwrap();
    assert_eq!(sha, "v2", "sha256 must be updated on upsert");

    teardown_tenant(&pool, &ctx).await;
}

#[tokio::test]
async fn write_source_file_rejects_tenant_mismatch() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx = new_ctx();
    setup_tenant(&pool, &ctx).await;

    let envelope_tid = TenantId::new();
    let different_tid = TenantId::new();
    let repo_id = uuid::Uuid::new_v4();
    let ev = SourceFileEvent {
        ingest_run_id: "run-1".to_string(),
        tenant_id: different_tid.to_string(),
        repo_id: repo_id.to_string(),
        relative_path: "src/main.rs".to_string(),
        sha256: "abc".to_string(),
        size_bytes: 100,
        emitted_at_ms: 0,
        body: None,
    };

    let result = projector_pg::write_source_file(&pool, &ctx, &envelope_tid, &ev).await;
    assert!(result.is_err(), "tenant mismatch must be rejected");

    teardown_tenant(&pool, &ctx).await;
}

#[tokio::test]
async fn write_source_file_with_blob_ref() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx = new_ctx();
    setup_tenant(&pool, &ctx).await;

    let tid = *ctx.tenant_id();
    let repo_id = uuid::Uuid::new_v4();
    let ev = SourceFileEvent {
        ingest_run_id: "run-1".to_string(),
        tenant_id: tid.to_string(),
        repo_id: repo_id.to_string(),
        relative_path: "src/large_file.rs".to_string(),
        sha256: "def456".to_string(),
        size_bytes: 1_000_000,
        emitted_at_ms: 0,
        body: Some(source_file_event::Body::BlobRef("rb-blob://test".to_string())),
    };

    projector_pg::write_source_file(&pool, &ctx, &tid, &ev)
        .await
        .expect("write with blob_ref");

    let blob: Option<String> = sqlx::query_scalar(&format!(
        "SELECT blob_ref FROM {}.code_files WHERE repo_id = $1 AND relative_path = $2",
        ctx.schema_name()
    ))
    .bind(repo_id)
    .bind("src/large_file.rs")
    .fetch_optional(pool.control())
    .await
    .unwrap()
    .flatten();
    assert_eq!(blob.as_deref(), Some("rb-blob://test"), "blob_ref must be stored");

    teardown_tenant(&pool, &ctx).await;
}

// ── parsed item projection ───────────────────────────────────────────────────

#[tokio::test]
async fn write_parsed_item_inserts_row() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx = new_ctx();
    setup_tenant(&pool, &ctx).await;

    let tid = *ctx.tenant_id();
    let repo_id = uuid::Uuid::new_v4();
    let ev = ParsedItemEvent {
        ingest_run_id: "run-1".to_string(),
        tenant_id: tid.to_string(),
        repo_id: repo_id.to_string(),
        fqn: "crate::module::my_function".to_string(),
        kind: ItemKind::Fn as i32,
        source_path: "src/module.rs".to_string(),
        line_start: 42,
        line_end: 55,
        emitted_at_ms: 0,
        body: None,
    };

    projector_pg::write_parsed_item(&pool, &ctx, &tid, &ev)
        .await
        .expect("write_parsed_item");

    let count: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {}.code_symbols",
        ctx.schema_name()
    ))
    .fetch_one(pool.control())
    .await
    .unwrap();
    assert_eq!(count, 1, "one symbol must be inserted");

    teardown_tenant(&pool, &ctx).await;
}

#[tokio::test]
async fn write_parsed_item_upsert_is_idempotent() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx = new_ctx();
    setup_tenant(&pool, &ctx).await;

    let tid = *ctx.tenant_id();
    let repo_id = uuid::Uuid::new_v4();
    let ev = ParsedItemEvent {
        ingest_run_id: "run-1".to_string(),
        tenant_id: tid.to_string(),
        repo_id: repo_id.to_string(),
        fqn: "crate::module::Foo".to_string(),
        kind: ItemKind::Struct as i32,
        source_path: "src/module.rs".to_string(),
        line_start: 10,
        line_end: 20,
        emitted_at_ms: 0,
        body: None,
    };

    projector_pg::write_parsed_item(&pool, &ctx, &tid, &ev)
        .await
        .expect("first write");

    let ev_v2 = ParsedItemEvent {
        line_start: 11,
        line_end: 21,
        ..ev.clone()
    };
    projector_pg::write_parsed_item(&pool, &ctx, &tid, &ev_v2)
        .await
        .expect("upsert");

    let count: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {}.code_symbols",
        ctx.schema_name()
    ))
    .fetch_one(pool.control())
    .await
    .unwrap();
    assert_eq!(count, 1, "upsert must not duplicate symbols");

    let (ls, le): (Option<i32>, Option<i32>) = sqlx::query_as(&format!(
        "SELECT line_start, line_end FROM {}.code_symbols WHERE repo_id = $1 AND fqn = $2",
        ctx.schema_name()
    ))
    .bind(repo_id)
    .bind("crate::module::Foo")
    .fetch_one(pool.control())
    .await
    .unwrap();
    assert_eq!(ls, Some(11), "line_start must be updated");
    assert_eq!(le, Some(21), "line_end must be updated");

    teardown_tenant(&pool, &ctx).await;
}

#[tokio::test]
async fn write_parsed_item_with_blob_ref() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx = new_ctx();
    setup_tenant(&pool, &ctx).await;

    let tid = *ctx.tenant_id();
    let repo_id = uuid::Uuid::new_v4();
    let ev = ParsedItemEvent {
        ingest_run_id: "run-1".to_string(),
        tenant_id: tid.to_string(),
        repo_id: repo_id.to_string(),
        fqn: "crate::module::large_fn".to_string(),
        kind: ItemKind::Fn as i32,
        source_path: "src/module.rs".to_string(),
        line_start: 1,
        line_end: 500,
        emitted_at_ms: 0,
        body: Some(parsed_item_event::Body::BlobRef("rb-blob://ast-json".to_string())),
    };

    projector_pg::write_parsed_item(&pool, &ctx, &tid, &ev)
        .await
        .expect("write with blob_ref");

    let blob: Option<String> = sqlx::query_scalar(&format!(
        "SELECT blob_ref FROM {}.code_symbols WHERE repo_id = $1 AND fqn = $2",
        ctx.schema_name()
    ))
    .bind(repo_id)
    .bind("crate::module::large_fn")
    .fetch_optional(pool.control())
    .await
    .unwrap()
    .flatten();
    assert_eq!(blob.as_deref(), Some("rb-blob://ast-json"));

    teardown_tenant(&pool, &ctx).await;
}

/// Regression for #274: re-ingestion must not overwrite source_text with NULL.
/// When body=None arrives, COALESCE in the UPSERT must preserve the original source_text.
#[tokio::test]
async fn write_parsed_item_reingest_preserves_source_text() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx = new_ctx();
    setup_tenant(&pool, &ctx).await;

    let tid = *ctx.tenant_id();
    let repo_id = uuid::Uuid::new_v4();

    // First ingestion: symbol with inline source_text payload.
    let ev_first = ParsedItemEvent {
        ingest_run_id: "run-1".to_string(),
        tenant_id: tid.to_string(),
        repo_id: repo_id.to_string(),
        fqn: "src_factor::ZERO_DECIMAL_PAIR".to_string(),
        kind: ItemKind::Const as i32,
        source_path: "src/factor.rs".to_string(),
        line_start: 3,
        line_end: 3,
        emitted_at_ms: 0,
        body: Some(parsed_item_event::Body::InlinePayload(
            b"pub const ZERO_DECIMAL_PAIR: (Decimal, Decimal) = (Decimal::ZERO, Decimal::ZERO);"
                .to_vec(),
        )),
    };
    projector_pg::write_parsed_item(&pool, &ctx, &tid, &ev_first)
        .await
        .expect("first ingestion");

    let source_text_after_first: Option<String> = sqlx::query_scalar(&format!(
        "SELECT source_text FROM {}.code_symbols WHERE repo_id = $1 AND fqn = $2",
        ctx.schema_name()
    ))
    .bind(repo_id)
    .bind("src_factor::ZERO_DECIMAL_PAIR")
    .fetch_one(pool.control())
    .await
    .unwrap();
    assert!(
        source_text_after_first.is_some(),
        "source_text must be set after first ingestion"
    );

    // Re-ingestion: same symbol but body=None (simulates the parse-worker edge case
    // where item_source_slice returns empty bytes and the proto oneof decodes as None).
    let ev_reingest = ParsedItemEvent {
        ingest_run_id: "run-2".to_string(),
        line_start: 3,
        line_end: 3,
        body: None,
        ..ev_first.clone()
    };
    projector_pg::write_parsed_item(&pool, &ctx, &tid, &ev_reingest)
        .await
        .expect("re-ingestion");

    let source_text_after_reingest: Option<String> = sqlx::query_scalar(&format!(
        "SELECT source_text FROM {}.code_symbols WHERE repo_id = $1 AND fqn = $2",
        ctx.schema_name()
    ))
    .bind(repo_id)
    .bind("src_factor::ZERO_DECIMAL_PAIR")
    .fetch_one(pool.control())
    .await
    .unwrap();
    assert_eq!(
        source_text_after_reingest, source_text_after_first,
        "re-ingestion with body=None must not overwrite source_text with NULL"
    );

    teardown_tenant(&pool, &ctx).await;
}

// ── relation projection ──────────────────────────────────────────────────────

#[tokio::test]
async fn write_relation_inserts_row() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx = new_ctx();
    setup_tenant(&pool, &ctx).await;

    let tid = *ctx.tenant_id();
    let repo_id = uuid::Uuid::new_v4();
    let ev = GraphRelationEvent {
        ingest_run_id: "run-1".to_string(),
        tenant_id: tid.to_string(),
        repo_id: repo_id.to_string(),
        from_fqn: "crate::module::foo".to_string(),
        to_fqn: "crate::module::bar".to_string(),
        kind: RelationKind::Calls as i32,
        emitted_at_ms: 0,
    };

    projector_pg::write_relation(&pool, &ctx, &tid, &ev)
        .await
        .expect("write_relation");

    let count: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {}.code_relations",
        ctx.schema_name()
    ))
    .fetch_one(pool.control())
    .await
    .unwrap();
    assert_eq!(count, 1, "one relation must be inserted");

    teardown_tenant(&pool, &ctx).await;
}

#[tokio::test]
async fn write_relation_dedup_is_idempotent() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx = new_ctx();
    setup_tenant(&pool, &ctx).await;

    let tid = *ctx.tenant_id();
    let repo_id = uuid::Uuid::new_v4();
    let ev = GraphRelationEvent {
        ingest_run_id: "run-1".to_string(),
        tenant_id: tid.to_string(),
        repo_id: repo_id.to_string(),
        from_fqn: "crate::module::A".to_string(),
        to_fqn: "crate::module::B".to_string(),
        kind: RelationKind::Impls as i32,
        emitted_at_ms: 0,
    };

    projector_pg::write_relation(&pool, &ctx, &tid, &ev)
        .await
        .expect("first write");
    projector_pg::write_relation(&pool, &ctx, &tid, &ev)
        .await
        .expect("second write (DO NOTHING)");

    let count: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {}.code_relations",
        ctx.schema_name()
    ))
    .fetch_one(pool.control())
    .await
    .unwrap();
    assert_eq!(count, 1, "DO NOTHING must not duplicate relations");

    teardown_tenant(&pool, &ctx).await;
}

#[tokio::test]
async fn write_relation_rejects_tenant_mismatch() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx = new_ctx();
    setup_tenant(&pool, &ctx).await;

    let envelope_tid = TenantId::new();
    let different_tid = TenantId::new();
    let repo_id = uuid::Uuid::new_v4();
    let ev = GraphRelationEvent {
        ingest_run_id: "run-1".to_string(),
        tenant_id: different_tid.to_string(),
        repo_id: repo_id.to_string(),
        from_fqn: "crate::A".to_string(),
        to_fqn: "crate::B".to_string(),
        kind: RelationKind::Calls as i32,
        emitted_at_ms: 0,
    };

    let result = projector_pg::write_relation(&pool, &ctx, &envelope_tid, &ev).await;
    assert!(result.is_err(), "tenant mismatch must be rejected");

    teardown_tenant(&pool, &ctx).await;
}

// ── cross-tenant isolation ───────────────────────────────────────────────────

#[tokio::test]
async fn projection_is_tenant_isolated() {
    skip_no_db!(url);
    let pool = make_pool(&url).await;
    let ctx_a = new_ctx();
    let ctx_b = new_ctx();
    setup_tenant(&pool, &ctx_a).await;
    setup_tenant(&pool, &ctx_b).await;

    let tid_a = *ctx_a.tenant_id();
    let tid_b = *ctx_b.tenant_id();
    let repo_id = uuid::Uuid::new_v4();

    let ev_a = SourceFileEvent {
        ingest_run_id: "run-1".to_string(),
        tenant_id: tid_a.to_string(),
        repo_id: repo_id.to_string(),
        relative_path: "src/a.rs".to_string(),
        sha256: "aaa".to_string(),
        size_bytes: 100,
        emitted_at_ms: 0,
        body: None,
    };
    projector_pg::write_source_file(&pool, &ctx_a, &tid_a, &ev_a)
        .await
        .expect("write to A");

    let ev_b = SourceFileEvent {
        ingest_run_id: "run-2".to_string(),
        tenant_id: tid_b.to_string(),
        repo_id: repo_id.to_string(),
        relative_path: "src/b.rs".to_string(),
        sha256: "bbb".to_string(),
        size_bytes: 200,
        emitted_at_ms: 0,
        body: None,
    };
    projector_pg::write_source_file(&pool, &ctx_b, &tid_b, &ev_b)
        .await
        .expect("write to B");

    let count_a: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {}.code_files",
        ctx_a.schema_name()
    ))
    .fetch_one(pool.control())
    .await
    .unwrap();
    let count_b: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {}.code_files",
        ctx_b.schema_name()
    ))
    .fetch_one(pool.control())
    .await
    .unwrap();
    assert_eq!(count_a, 1, "tenant A must have exactly 1 row");
    assert_eq!(count_b, 1, "tenant B must have exactly 1 row");

    teardown_tenant(&pool, &ctx_a).await;
    teardown_tenant(&pool, &ctx_b).await;
}
