//! `rb-query::pg::items` — code-symbol lookup queries (ADR-008 §4.1).
//!
//! All queries use fully-qualified table names via [`TenantCtx::qualify`];
//! no `search_path` manipulation is ever performed.

use rb_tenant::TenantCtx;
use sqlx::PgPool;
use uuid::Uuid;

/// A code symbol row fetched from the tenant's `code_symbols` table.
#[derive(Debug, Clone)]
pub struct CodeSymbol {
    pub id: Uuid,
    pub fqn: String,
    pub kind: String,
    pub source_path: Option<String>,
    pub line_start: Option<i32>,
    pub line_end: Option<i32>,
    /// `rb-blob://` URI pointing to the serialised AST JSON.
    /// Present only for items whose source exceeds the inline threshold.
    pub blob_ref: Option<String>,
}

/// Look up a single code symbol by `(repo_id, fqn)` within `ctx`'s schema.
///
/// Returns `None` when the `(repo, fqn)` tuple is absent (AC2 — 404 path).
///
/// The caller must independently verify that `repo_id` belongs to the tenant
/// identified by `ctx` before invoking this function (AC4).
///
/// # Errors
///
/// Returns [`sqlx::Error`] on database failure.
pub async fn get_by_fqn(
    pool: &PgPool,
    ctx: &TenantCtx,
    repo_id: Uuid,
    fqn: &str,
) -> Result<Option<CodeSymbol>, sqlx::Error> {
    let table = ctx.qualify("code_symbols");
    let row: Option<(Uuid, String, String, Option<String>, Option<i32>, Option<i32>, Option<String>)> =
        sqlx::query_as(&format!(
            "SELECT id, fqn, kind, source_path, line_start, line_end, blob_ref \
             FROM {table} \
             WHERE repo_id = $1 AND fqn = $2",
        ))
        .bind(repo_id)
        .bind(fqn)
        .fetch_optional(pool)
        .await?;

    Ok(row.map(|(id, fqn, kind, source_path, line_start, line_end, blob_ref)| {
        CodeSymbol { id, fqn, kind, source_path, line_start, line_end, blob_ref }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_symbol_fields_are_accessible() {
        let sym = CodeSymbol {
            id: Uuid::new_v4(),
            fqn: "my_crate::my_mod::MyStruct".to_owned(),
            kind: "STRUCT".to_owned(),
            source_path: Some("src/my_mod.rs".to_owned()),
            line_start: Some(10),
            line_end: Some(25),
            blob_ref: None,
        };
        assert_eq!(sym.kind, "STRUCT");
        assert!(sym.blob_ref.is_none());
    }

    #[test]
    fn code_symbol_with_blob_ref() {
        let sym = CodeSymbol {
            id: Uuid::new_v4(),
            fqn: "my_crate::huge_fn".to_owned(),
            kind: "FN".to_owned(),
            source_path: Some("src/lib.rs".to_owned()),
            line_start: Some(1),
            line_end: Some(500),
            blob_ref: Some("rb-blob://tenant_abc123/items/uuid123.json".to_owned()),
        };
        assert!(sym.blob_ref.is_some());
    }
}
