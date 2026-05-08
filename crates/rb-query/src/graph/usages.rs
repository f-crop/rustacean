//! Type-usage graph queries (REQ-DP-04 / ADR-008 §3.4).
//!
//! Two sources of usage for a type `fqn`:
//!   - Textual (`USES_TYPE` edge)     — items that directly reference the type
//!     by name in their source (an `Item → Item` edge in the graph).
//!   - Monomorphized (`MONOMORPHIZED_FROM` edge) — concrete `TypeInstance`
//!     nodes that were instantiated from this `TypeDef`.

use rb_schemas::TenantId;
use rb_storage_neo4j::TenantGraph;
use uuid::Uuid;

use crate::QueryError;

/// A single item that uses the queried type.
#[derive(Debug, Clone)]
pub struct UsageEntry {
    /// Fully-qualified name of the using item or type instance.
    pub fqn: String,
    /// `"textual"` for `USES_TYPE` edges; `"monomorphized"` for
    /// `MONOMORPHIZED_FROM` edges from a `TypeInstance` node.
    pub usage_kind: String,
}

/// Fetch all usages of type `fqn` within `repo_id`.
///
/// Returns two disjoint sets:
/// - **Textual** usages: `Item` nodes connected by `USES_TYPE` to the `Item`
///   node for `fqn`.
/// - **Monomorphized** usages: `TypeInstance` nodes connected by
///   `MONOMORPHIZED_FROM` to the `TypeDef` node for `fqn`.
///
/// The two sets are combined with `UNION` so the caller receives a single
/// flat list.  Duplicates across the two result sets are removed by Neo4j.
///
/// Returns an empty `Vec` when the graph holds no recorded usages.
///
/// # Errors
///
/// Propagates [`QueryError::Graph`] on driver or injection failure.
pub async fn fetch_type_usages(
    graph: &TenantGraph,
    tenant_id: &TenantId,
    repo_id: Uuid,
    fqn: &str,
) -> Result<Vec<UsageEntry>, QueryError> {
    let repo_str = repo_id.to_string();

    // UNION deduplicates rows that appear in both branches (defensive — in
    // practice textual Item usages and TypeInstance monomorphizations are
    // disjoint sets).
    let cypher = "\
        MATCH (user:Item {repo_id: $repo_id})-[:USES_TYPE]->(t:Item {fqn: $fqn, repo_id: $repo_id}) \
        RETURN user.fqn AS fqn, 'textual' AS usage_kind \
        UNION \
        MATCH (inst:TypeInstance {repo_id: $repo_id})-[:MONOMORPHIZED_FROM]->(t:TypeDef {fqn: $fqn, repo_id: $repo_id}) \
        RETURN inst.fqn AS fqn, 'monomorphized' AS usage_kind";

    let rows = graph
        .execute_read(tenant_id, cypher, &[("fqn", fqn), ("repo_id", &repo_str)])
        .await?;

    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(fqn_val) = row.get::<String>("fqn").ok() else {
            continue;
        };
        let usage_kind = row
            .get::<String>("usage_kind")
            .unwrap_or_else(|_| "textual".to_owned());
        entries.push(UsageEntry {
            fqn: fqn_val,
            usage_kind,
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_entry_textual_kind() {
        let entry = UsageEntry {
            fqn: "my_crate::uses_foo".to_owned(),
            usage_kind: "textual".to_owned(),
        };
        assert_eq!(entry.fqn, "my_crate::uses_foo");
        assert_eq!(entry.usage_kind, "textual");
    }

    #[test]
    fn usage_entry_monomorphized_kind() {
        let entry = UsageEntry {
            fqn: "my_crate::Foo<i32>".to_owned(),
            usage_kind: "monomorphized".to_owned(),
        };
        assert_eq!(entry.usage_kind, "monomorphized");
    }
}
