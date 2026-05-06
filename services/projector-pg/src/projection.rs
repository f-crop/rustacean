//! `PostgreSQL` projection logic for item and relation events.
//!
//! All writes go through `TenantPool` with fully-qualified table names
//! (ADR-007 §11.9 / REQ-IN-09).

use anyhow::{Context as _, Result};
use rb_schemas::{ParsedItemEvent, SourceFileEvent, GraphRelationEvent, ItemKind, RelationKind, TenantId};
use rb_schemas::{source_file_event, parsed_item_event};
use rb_storage_pg::{StorageError, TenantPool};
use rb_tenant::TenantCtx;

#[derive(Debug, thiserror::Error)]
pub enum ProjectionError {
    #[error("tenant mismatch: envelope={envelope}, event={event}")]
    TenantMismatch { envelope: String, event: String },
}

fn verify_tenant(envelope_tenant: &TenantId, event_tenant: &str) -> Result<(), ProjectionError> {
    if envelope_tenant.to_string() != event_tenant {
        return Err(ProjectionError::TenantMismatch {
            envelope: envelope_tenant.to_string(),
            event: event_tenant.to_owned(),
        });
    }
    Ok(())
}

/// Write a `SourceFileEvent` to the tenant's `code_files` table.
///
/// Idempotent: uses `ON CONFLICT (repo_id, relative_path) DO UPDATE`.
#[allow(clippy::missing_errors_doc)]
pub async fn write_source_file(
    pool: &TenantPool,
    tenant_ctx: &TenantCtx,
    envelope_tenant: &TenantId,
    ev: &SourceFileEvent,
) -> Result<()> {
    verify_tenant(envelope_tenant, &ev.tenant_id)?;
    let table = tenant_ctx.qualify("code_files");
    let blob_ref = match &ev.body {
        Some(source_file_event::Body::BlobRef(s)) => Some(s.as_str()),
        _ => None,
    };

    let repo_id = parse_uuid(&ev.repo_id)
        .context("invalid repo_id in SourceFileEvent")?;

    sqlx::query(&format!(
        r"INSERT INTO {table} (repo_id, relative_path, sha256, size_bytes, blob_ref)
           VALUES ($1, $2, $3, $4, $5)
           ON CONFLICT (repo_id, relative_path) DO UPDATE SET
               sha256 = EXCLUDED.sha256,
               size_bytes = EXCLUDED.size_bytes,
               blob_ref = EXCLUDED.blob_ref,
               updated_at = now()"
    ))
    .bind(repo_id)
    .bind(&ev.relative_path)
    .bind(&ev.sha256)
    .bind(ev.size_bytes)
    .bind(blob_ref)
    .execute(pool.control())
    .await
    .map_err(StorageError::Sqlx)
    .context("failed to upsert source file")?;

    Ok(())
}

/// Write a `ParsedItemEvent` to the tenant's `code_symbols` table.
///
/// Idempotent: uses `ON CONFLICT (repo_id, fqn) DO UPDATE`.
#[allow(clippy::missing_errors_doc)]
pub async fn write_parsed_item(
    pool: &TenantPool,
    tenant_ctx: &TenantCtx,
    envelope_tenant: &TenantId,
    ev: &ParsedItemEvent,
) -> Result<()> {
    verify_tenant(envelope_tenant, &ev.tenant_id)?;
    let table = tenant_ctx.qualify("code_symbols");
    let kind = item_kind_str(ItemKind::try_from(ev.kind).unwrap_or(ItemKind::Unspecified));
    let (source_text, blob_ref): (Option<String>, Option<&str>) = match &ev.body {
        Some(parsed_item_event::Body::BlobRef(s)) => (None, Some(s.as_str())),
        Some(parsed_item_event::Body::InlinePayload(bytes)) => {
            (Some(String::from_utf8_lossy(bytes).into_owned()), None)
        }
        None => (None, None),
    };

    let repo_id = parse_uuid(&ev.repo_id)
        .context("invalid repo_id in ParsedItemEvent")?;

    sqlx::query(&format!(
        r"INSERT INTO {table} (repo_id, fqn, kind, source_path, line_start, line_end, blob_ref, source_text)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
           ON CONFLICT (repo_id, fqn) DO UPDATE SET
               kind = EXCLUDED.kind,
               source_path = EXCLUDED.source_path,
               line_start = EXCLUDED.line_start,
               line_end = EXCLUDED.line_end,
               blob_ref = EXCLUDED.blob_ref,
               source_text = COALESCE(EXCLUDED.source_text, {table}.source_text),
               updated_at = now()"
    ))
    .bind(repo_id)
    .bind(&ev.fqn)
    .bind(kind)
    .bind(&ev.source_path)
    .bind(ev.line_start)
    .bind(ev.line_end)
    .bind(blob_ref)
    .bind(source_text)
    .execute(pool.control())
    .await
    .map_err(StorageError::Sqlx)
    .context("failed to upsert parsed item")?;

    Ok(())
}

/// Write a `GraphRelationEvent` to the tenant's `code_relations` table.
///
/// Idempotent: uses `ON CONFLICT (repo_id, from_fqn, to_fqn, kind) DO NOTHING`.
#[allow(clippy::missing_errors_doc)]
pub async fn write_relation(
    pool: &TenantPool,
    tenant_ctx: &TenantCtx,
    envelope_tenant: &TenantId,
    ev: &GraphRelationEvent,
) -> Result<()> {
    verify_tenant(envelope_tenant, &ev.tenant_id)?;
    let table = tenant_ctx.qualify("code_relations");
    let kind = relation_kind_str(
        RelationKind::try_from(ev.kind).unwrap_or(RelationKind::Unspecified)
    );

    let repo_id = parse_uuid(&ev.repo_id)
        .context("invalid repo_id in GraphRelationEvent")?;

    sqlx::query(&format!(
        r"INSERT INTO {table} (repo_id, from_fqn, to_fqn, kind)
           VALUES ($1, $2, $3, $4)
           ON CONFLICT (repo_id, from_fqn, to_fqn, kind) DO NOTHING"
    ))
    .bind(repo_id)
    .bind(&ev.from_fqn)
    .bind(&ev.to_fqn)
    .bind(kind)
    .execute(pool.control())
    .await
    .map_err(StorageError::Sqlx)
    .context("failed to upsert relation")?;

    Ok(())
}

/// Parse a UUID string, returning a sqlx-friendly type.
fn parse_uuid(s: &str) -> Result<uuid::Uuid> {
    s.parse::<uuid::Uuid>().context("invalid UUID format")
}

/// Map `ItemKind` to the database string representation.
fn item_kind_str(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Fn => "FN",
        ItemKind::Struct => "STRUCT",
        ItemKind::Enum => "ENUM",
        ItemKind::Trait => "TRAIT",
        ItemKind::Impl => "IMPL",
        ItemKind::Mod => "MOD",
        ItemKind::Const => "CONST",
        ItemKind::TraitMethod => "TRAIT_METHOD",
        ItemKind::ImplBlock => "IMPL_BLOCK",
        ItemKind::AssocType => "ASSOC_TYPE",
        ItemKind::AssocConst => "ASSOC_CONST",
        ItemKind::TypeParam => "TYPE_PARAM",
        ItemKind::LifetimeParam => "LIFETIME_PARAM",
        ItemKind::ConstParam => "CONST_PARAM",
        ItemKind::WherePredicate => "WHERE_PREDICATE",
        ItemKind::DynTraitUsage => "DYN_TRAIT_USAGE",
        ItemKind::TypeInstance => "TYPE_INSTANCE",
        ItemKind::MacroDef => "MACRO_DEF",
        ItemKind::Unspecified => "UNSPECIFIED",
    }
}

/// Map `RelationKind` to the database string representation.
fn relation_kind_str(kind: RelationKind) -> &'static str {
    match kind {
        RelationKind::Calls => "CALLS",
        RelationKind::Impls => "IMPLS",
        RelationKind::UsesType => "USES_TYPE",
        RelationKind::Imports => "IMPORTS",
        RelationKind::Derives => "DERIVES",
        RelationKind::BoundedBy => "BOUNDED_BY",
        RelationKind::WhereClausePredicate => "WHERE_CLAUSE_PREDICATE",
        RelationKind::ExtendsTrait => "EXTENDS_TRAIT",
        RelationKind::BlanketImplFor => "BLANKET_IMPL_FOR",
        RelationKind::AssocTypeBinding => "ASSOC_TYPE_BINDING",
        RelationKind::HasTypeParam => "HAS_TYPE_PARAM",
        RelationKind::HasLifetimeParam => "HAS_LIFETIME_PARAM",
        RelationKind::HasConstParam => "HAS_CONST_PARAM",
        RelationKind::MonomorphizedFrom => "MONOMORPHIZED_FROM",
        RelationKind::TypeArgBinds => "TYPE_ARG_BINDS",
        RelationKind::CallInstantiates => "CALL_INSTANTIATES",
        RelationKind::MacroGeneratedBy => "MACRO_GENERATED_BY",
        RelationKind::DynDispatchCandidate => "DYN_DISPATCH_CANDIDATE",
        RelationKind::ErasedInto => "ERASED_INTO",
        RelationKind::Unspecified => "UNSPECIFIED",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rb_schemas::TenantId as SchemasTenantId;

    #[test]
    fn item_kind_str_covers_all_stable_kinds() {
        for kind in [
            ItemKind::Fn,
            ItemKind::Struct,
            ItemKind::Enum,
            ItemKind::Trait,
            ItemKind::Impl,
            ItemKind::Mod,
            ItemKind::Const,
        ] {
            let s = item_kind_str(kind);
            assert!(!s.is_empty());
            assert!(s.chars().all(|c| c.is_ascii_uppercase() || c == '_'));
        }
    }

    #[test]
    fn relation_kind_str_covers_all_stable_kinds() {
        for kind in [
            RelationKind::Calls,
            RelationKind::Impls,
            RelationKind::UsesType,
            RelationKind::Imports,
            RelationKind::Derives,
        ] {
            let s = relation_kind_str(kind);
            assert!(!s.is_empty());
            assert!(s.chars().all(|c| c.is_ascii_uppercase() || c == '_'));
        }
    }

    #[test]
    fn verify_tenant_rejects_mismatch() {
        let envelope_tid = SchemasTenantId::new();
        let event_tid = SchemasTenantId::new();
        let result = verify_tenant(&envelope_tid, &event_tid.to_string());
        assert!(result.is_err(), "mismatched tenants must be rejected");
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("tenant mismatch"), "expected tenant mismatch, got: {err_msg}");
    }

    #[test]
    fn verify_tenant_accepts_matching() {
        let tid = SchemasTenantId::new();
        let result = verify_tenant(&tid, &tid.to_string());
        assert!(result.is_ok(), "matching tenants must be accepted");
    }

    #[test]
    fn parse_uuid_rejects_invalid() {
        let result = parse_uuid("not-a-uuid");
        assert!(result.is_err());
    }

    #[test]
    fn parse_uuid_accepts_valid() {
        let uid = uuid::Uuid::new_v4();
        let result = parse_uuid(&uid.to_string());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), uid);
    }
}
