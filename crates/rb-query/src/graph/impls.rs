//! Trait-impl graph queries (REQ-DP-04 / ADR-008 §3.4).
//!
//! Two relationship types target a trait node from the same `repo_id`:
//!   - `IMPLS`            — direct concrete impls (`impl Display for Foo`)
//!   - `BLANKET_IMPL_FOR` — blanket impls (`impl<T: Sized> Display for T`)

use rb_schemas::TenantId;
use rb_storage_neo4j::TenantGraph;
use uuid::Uuid;

use crate::QueryError;

/// A single impl block that implements the queried trait.
#[derive(Debug, Clone)]
pub struct ImplEntry {
    /// Fully-qualified name of the impl block.
    pub fqn: String,
    /// `"direct"` for `IMPLS` edges; `"blanket"` for `BLANKET_IMPL_FOR` edges.
    pub impl_kind: String,
}

/// Fetch all impl blocks for trait `fqn` within `repo_id`.
///
/// Returns direct impls (`IMPLS` edge) and blanket impls (`BLANKET_IMPL_FOR`
/// edge).  Both are represented as `Item` nodes in the graph.  Rows are
/// de-duplicated by Neo4j's `UNION` (no blanket impl counts as direct or
/// vice-versa for the same fqn).
///
/// Returns an empty `Vec` when the graph holds no impls for the given trait.
///
/// # Errors
///
/// Propagates [`QueryError::Graph`] on driver or injection failure.
pub async fn fetch_trait_impls(
    graph: &TenantGraph,
    tenant_id: &TenantId,
    repo_id: Uuid,
    fqn: &str,
) -> Result<Vec<ImplEntry>, QueryError> {
    let repo_str = repo_id.to_string();

    // UNION deduplicates; direct and blanket sets should never overlap for a
    // single impl block, but the dedup is a safety net.
    let cypher = "\
        MATCH (impl:Item {repo_id: $repo_id})-[:IMPLS]->(t:Item {fqn: $fqn, repo_id: $repo_id}) \
        RETURN impl.fqn AS fqn, 'direct' AS impl_kind \
        UNION \
        MATCH (impl:Item {repo_id: $repo_id})-[:BLANKET_IMPL_FOR]->(t:Item {fqn: $fqn, repo_id: $repo_id}) \
        RETURN impl.fqn AS fqn, 'blanket' AS impl_kind";

    let rows = graph
        .execute_read(tenant_id, cypher, &[("fqn", fqn), ("repo_id", &repo_str)])
        .await?;

    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(fqn_val) = row.get::<String>("fqn").ok() else {
            continue;
        };
        let impl_kind = row.get::<String>("impl_kind").unwrap_or_else(|_| "direct".to_owned());
        entries.push(ImplEntry { fqn: fqn_val, impl_kind });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impl_entry_fields_are_accessible() {
        let entry = ImplEntry { fqn: "my_crate::Foo".to_owned(), impl_kind: "direct".to_owned() };
        assert_eq!(entry.fqn, "my_crate::Foo");
        assert_eq!(entry.impl_kind, "direct");
    }

    #[test]
    fn impl_entry_blanket_kind() {
        let entry =
            ImplEntry { fqn: "my_crate::GenericFoo".to_owned(), impl_kind: "blanket".to_owned() };
        assert_eq!(entry.impl_kind, "blanket");
    }
}
