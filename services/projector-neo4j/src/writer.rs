//! Neo4j write logic for graph relations.
//!
//! All Cypher passes through `rb_storage_neo4j::TenantGraph`, which injects the
//! tenant label before execution (ADR-007 §3.4 / REQ-IN-14).

use std::hash::{Hash, Hasher};

use siphasher::sip::SipHasher13;

use rb_schemas::{GraphRelationEvent, RelationKind, TenantId};
use rb_storage_neo4j::{CypherError, TenantGraph};

/// Write a `GraphRelationEvent` to Neo4j.
///
/// Selects node labels and relationship type from `ev.kind`, then issues
/// MERGE-based Cypher through `TenantGraph::run` so replays are idempotent.
///
/// # Errors
///
/// Propagates [`CypherError`] from `TenantGraph::run`.
pub async fn write_relation(
    graph: &TenantGraph,
    tenant_id: &TenantId,
    ev: &GraphRelationEvent,
) -> Result<(), CypherError> {
    let kind = RelationKind::try_from(ev.kind).unwrap_or(RelationKind::Unspecified);

    match kind {
        RelationKind::MonomorphizedFrom | RelationKind::TypeArgBinds => {
            write_type_instance_relation(graph, tenant_id, ev, kind).await
        }
        RelationKind::DynDispatchCandidate => {
            write_dyn_dispatch_relation(graph, tenant_id, ev).await
        }
        _ => write_item_relation(graph, tenant_id, ev, kind).await,
    }
}

// ── TypeInstance / TypeDef two-level model ────────────────────────────────────

async fn write_type_instance_relation(
    graph: &TenantGraph,
    tenant_id: &TenantId,
    ev: &GraphRelationEvent,
    kind: RelationKind,
) -> Result<(), CypherError> {
    let type_arg_hash = hash_fqn(&ev.from_fqn);
    let rel_type = relation_type_str(kind);

    // MERGE the TypeInstance (source) node.
    graph
        .run(
            tenant_id,
            "MERGE (n:TypeInstance {fqn: $fqn, repo_id: $repo_id, \
             def_fqn: $def_fqn, type_arg_hash: $hash})",
            &[
                ("fqn", ev.from_fqn.as_str()),
                ("repo_id", ev.repo_id.as_str()),
                ("def_fqn", ev.to_fqn.as_str()),
                ("hash", type_arg_hash.as_str()),
            ],
        )
        .await?;

    // MERGE the TypeDef (target) node.
    graph
        .run(
            tenant_id,
            "MERGE (n:TypeDef {fqn: $fqn, repo_id: $repo_id})",
            &[
                ("fqn", ev.to_fqn.as_str()),
                ("repo_id", ev.repo_id.as_str()),
            ],
        )
        .await?;

    // MERGE the relationship.
    let rel_cypher = format!(
        "MATCH (a:TypeInstance {{fqn: $from_fqn, repo_id: $repo_id}}) \
         MATCH (b:TypeDef {{fqn: $to_fqn, repo_id: $repo_id}}) \
         MERGE (a)-[:{rel_type}]->(b)"
    );
    graph
        .run(
            tenant_id,
            &rel_cypher,
            &[
                ("from_fqn", ev.from_fqn.as_str()),
                ("to_fqn", ev.to_fqn.as_str()),
                ("repo_id", ev.repo_id.as_str()),
            ],
        )
        .await
}

// ── Dyn-trait dispatch model (closed-world) ───────────────────────────────────

async fn write_dyn_dispatch_relation(
    graph: &TenantGraph,
    tenant_id: &TenantId,
    ev: &GraphRelationEvent,
) -> Result<(), CypherError> {
    // MERGE the DynTraitUsage node, setting world='closed' on creation.
    graph
        .run(
            tenant_id,
            "MERGE (n:DynTraitUsage {fqn: $fqn, repo_id: $repo_id}) \
             ON CREATE SET n.world = 'closed'",
            &[
                ("fqn", ev.from_fqn.as_str()),
                ("repo_id", ev.repo_id.as_str()),
            ],
        )
        .await?;

    // MERGE the ImplBlock (target) node.
    graph
        .run(
            tenant_id,
            "MERGE (n:ImplBlock {fqn: $fqn, repo_id: $repo_id})",
            &[
                ("fqn", ev.to_fqn.as_str()),
                ("repo_id", ev.repo_id.as_str()),
            ],
        )
        .await?;

    // MERGE the relationship.
    graph
        .run(
            tenant_id,
            "MATCH (a:DynTraitUsage {fqn: $from_fqn, repo_id: $repo_id}) \
             MATCH (b:ImplBlock {fqn: $to_fqn, repo_id: $repo_id}) \
             MERGE (a)-[:DYN_DISPATCH_CANDIDATE]->(b)",
            &[
                ("from_fqn", ev.from_fqn.as_str()),
                ("to_fqn", ev.to_fqn.as_str()),
                ("repo_id", ev.repo_id.as_str()),
            ],
        )
        .await
}

// ── Generic Item ──────────────────────────────────────────────────────────────

async fn write_item_relation(
    graph: &TenantGraph,
    tenant_id: &TenantId,
    ev: &GraphRelationEvent,
    kind: RelationKind,
) -> Result<(), CypherError> {
    // MERGE both endpoint nodes.
    graph
        .run(
            tenant_id,
            "MERGE (n:Item {fqn: $fqn, repo_id: $repo_id})",
            &[
                ("fqn", ev.from_fqn.as_str()),
                ("repo_id", ev.repo_id.as_str()),
            ],
        )
        .await?;
    graph
        .run(
            tenant_id,
            "MERGE (n:Item {fqn: $fqn, repo_id: $repo_id})",
            &[
                ("fqn", ev.to_fqn.as_str()),
                ("repo_id", ev.repo_id.as_str()),
            ],
        )
        .await?;

    // MERGE the typed relationship.
    let rel_type = relation_type_str(kind);
    let rel_cypher = format!(
        "MATCH (a:Item {{fqn: $from_fqn, repo_id: $repo_id}}) \
         MATCH (b:Item {{fqn: $to_fqn, repo_id: $repo_id}}) \
         MERGE (a)-[:{rel_type}]->(b)"
    );
    graph
        .run(
            tenant_id,
            &rel_cypher,
            &[
                ("from_fqn", ev.from_fqn.as_str()),
                ("to_fqn", ev.to_fqn.as_str()),
                ("repo_id", ev.repo_id.as_str()),
            ],
        )
        .await
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Map `RelationKind` to the Cypher relationship-type string.
///
/// These are embedded directly into Cypher (not parameterised — Neo4j does not
/// support parameterised relationship types).  Values are ASCII uppercase
/// identifiers and therefore safe to interpolate.
#[allow(clippy::too_many_lines)]
fn relation_type_str(kind: RelationKind) -> &'static str {
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
        RelationKind::Unspecified => "UNKNOWN",
    }
}

/// Deterministic 16-hex-char hash of `fqn`, used as `type_arg_hash` for `TypeInstance` dedup.
///
/// The full FQN encodes the concrete type arguments (e.g. `Vec<i32>::push`), so
/// hashing it gives a unique-per-instantiation key (ADR-007 §11.10).
///
/// Uses `SipHasher13` with fixed keys so the output is stable across Rust toolchain
/// versions (unlike `DefaultHasher` whose impl is explicitly unspecified).
fn hash_fqn(fqn: &str) -> String {
    let mut h = SipHasher13::new_with_keys(0, 0);
    fqn.hash(&mut h);
    format!("{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_fqn_pinned_value() {
        // Pin the exact byte output of SipHasher13(k0=0, k1=0) for "std::vec::Vec<i32>".
        // If this test ever fails after a toolchain upgrade the implementation regressed to
        // a non-stable hasher — fix the hasher, not this constant.
        assert_eq!(hash_fqn("std::vec::Vec<i32>"), "4e91803ce2caff87");
    }

    #[test]
    fn hash_fqn_is_deterministic() {
        assert_eq!(
            hash_fqn("std::vec::Vec<i32>"),
            hash_fqn("std::vec::Vec<i32>")
        );
    }

    #[test]
    fn hash_fqn_differs_for_different_inputs() {
        assert_ne!(
            hash_fqn("std::vec::Vec<i32>"),
            hash_fqn("std::vec::Vec<u64>")
        );
    }

    #[test]
    fn hash_fqn_is_16_hex_chars() {
        let h = hash_fqn("any::fqn");
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn relation_type_str_covers_all_stable_kinds() {
        for kind in [
            RelationKind::Calls,
            RelationKind::Impls,
            RelationKind::UsesType,
            RelationKind::Imports,
            RelationKind::Derives,
            RelationKind::MonomorphizedFrom,
            RelationKind::TypeArgBinds,
            RelationKind::DynDispatchCandidate,
        ] {
            let s = relation_type_str(kind);
            assert!(!s.is_empty());
            assert!(s.chars().all(|c| c.is_ascii_uppercase() || c == '_'));
        }
    }
}
