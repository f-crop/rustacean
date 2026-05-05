use std::collections::HashSet;

use rb_schemas::TenantId;
use rb_storage_neo4j::TenantGraph;

use crate::QueryError;
use super::types::{EdgeProvenance, TraversalNode};

// ---------------------------------------------------------------------------
// Internal edge accumulator (before pagination)
// ---------------------------------------------------------------------------

pub(super) struct RawEdge {
    pub(super) from_fqn: String,
    pub(super) to_fqn: String,
    pub(super) depth: u32,
    pub(super) provenance: EdgeProvenance,
    /// Source node info (the "from" side).
    pub(super) from_node: TraversalNode,
}

// ---------------------------------------------------------------------------
// BFS helpers
// ---------------------------------------------------------------------------

/// Query one hop of callers: all `Item` nodes that directly call any FQN in
/// `frontier` within `repo_id`.
///
/// Returns raw edge info for each `CALLS` or `CALL_INSTANTIATES` relationship.
/// Queries `frontier` entries individually to stay within the string-param
/// API of [`TenantGraph::execute_read`].
pub(super) async fn one_hop_callers(
    graph: &TenantGraph,
    tenant_id: &TenantId,
    repo_str: &str,
    frontier: &[String],
    depth: u32,
    visited: &HashSet<String>,
    cycles_detected: &mut bool,
) -> Result<Vec<RawEdge>, QueryError> {
    let mut edges = Vec::new();

    for target_fqn in frontier {
        // Direct + monomorphized callers via CALLS
        let calls_cypher = "\
            MATCH (caller:Item {repo_id: $repo_id})-[r:CALLS]->(target:Item {fqn: $fqn, repo_id: $repo_id}) \
            RETURN caller.fqn AS caller_fqn, caller.name AS caller_name, \
                   caller.kind AS caller_kind, caller.file_path AS caller_file, \
                   caller.line AS caller_line, r.dispatch AS dispatch";

        let rows = graph
            .execute_read(tenant_id, calls_cypher, &[("fqn", target_fqn), ("repo_id", repo_str)])
            .await?;

        for row in rows {
            let Some(caller_fqn) = row.get::<String>("caller_fqn").ok() else { continue };
            if visited.contains(&caller_fqn) {
                *cycles_detected = true;
                continue;
            }
            let dispatch = row.get::<String>("dispatch").ok();
            let provenance = match dispatch.as_deref() {
                Some("dynamic") => EdgeProvenance::DynCandidate,
                _ => EdgeProvenance::Direct,
            };
            edges.push(RawEdge {
                from_fqn: caller_fqn.clone(),
                to_fqn: target_fqn.clone(),
                depth,
                provenance,
                from_node: TraversalNode {
                    fqn: caller_fqn,
                    name: row.get::<String>("caller_name").ok(),
                    kind: row.get::<String>("caller_kind").ok(),
                    file_path: row.get::<String>("caller_file").ok(),
                    line: row.get::<i64>("caller_line").ok(),
                },
            });
        }

        // Monomorphized callers via CALL_INSTANTIATES
        let inst_cypher = "\
            MATCH (caller:Item {repo_id: $repo_id})-[:CALL_INSTANTIATES]->(target:Item {fqn: $fqn, repo_id: $repo_id}) \
            RETURN caller.fqn AS caller_fqn, caller.name AS caller_name, \
                   caller.kind AS caller_kind, caller.file_path AS caller_file, \
                   caller.line AS caller_line";

        let inst_rows = graph
            .execute_read(tenant_id, inst_cypher, &[("fqn", target_fqn), ("repo_id", repo_str)])
            .await?;

        for row in inst_rows {
            let Some(caller_fqn) = row.get::<String>("caller_fqn").ok() else { continue };
            if visited.contains(&caller_fqn) {
                *cycles_detected = true;
                continue;
            }
            edges.push(RawEdge {
                from_fqn: caller_fqn.clone(),
                to_fqn: target_fqn.clone(),
                depth,
                provenance: EdgeProvenance::Monomorph,
                from_node: TraversalNode {
                    fqn: caller_fqn,
                    name: row.get::<String>("caller_name").ok(),
                    kind: row.get::<String>("caller_kind").ok(),
                    file_path: row.get::<String>("caller_file").ok(),
                    line: row.get::<i64>("caller_line").ok(),
                },
            });
        }
    }

    Ok(edges)
}

/// Query one hop of callees: all `Item` nodes directly called by any FQN in
/// `frontier` within `repo_id`.
pub(super) async fn one_hop_callees(
    graph: &TenantGraph,
    tenant_id: &TenantId,
    repo_str: &str,
    frontier: &[String],
    depth: u32,
    visited: &HashSet<String>,
    cycles_detected: &mut bool,
) -> Result<Vec<RawEdge>, QueryError> {
    let mut edges = Vec::new();

    for source_fqn in frontier {
        let calls_cypher = "\
            MATCH (source:Item {fqn: $fqn, repo_id: $repo_id})-[r:CALLS]->(callee:Item {repo_id: $repo_id}) \
            RETURN callee.fqn AS callee_fqn, callee.name AS callee_name, \
                   callee.kind AS callee_kind, callee.file_path AS callee_file, \
                   callee.line AS callee_line, r.dispatch AS dispatch";

        let rows = graph
            .execute_read(tenant_id, calls_cypher, &[("fqn", source_fqn), ("repo_id", repo_str)])
            .await?;

        for row in rows {
            let Some(callee_fqn) = row.get::<String>("callee_fqn").ok() else { continue };
            if visited.contains(&callee_fqn) {
                *cycles_detected = true;
                continue;
            }
            let dispatch = row.get::<String>("dispatch").ok();
            let provenance = match dispatch.as_deref() {
                Some("dynamic") => EdgeProvenance::DynCandidate,
                _ => EdgeProvenance::Direct,
            };
            edges.push(RawEdge {
                from_fqn: source_fqn.clone(),
                to_fqn: callee_fqn.clone(),
                depth,
                provenance,
                from_node: TraversalNode {
                    fqn: source_fqn.clone(),
                    name: None,
                    kind: None,
                    file_path: None,
                    line: None,
                },
            });
            // Populate the callee node info (it's the "to" side here).
            // We'll collect node info from the callee side separately below.
            let _ = row.get::<String>("callee_name");
        }

        let inst_cypher = "\
            MATCH (source:Item {fqn: $fqn, repo_id: $repo_id})-[:CALL_INSTANTIATES]->(callee:Item {repo_id: $repo_id}) \
            RETURN callee.fqn AS callee_fqn, callee.name AS callee_name, \
                   callee.kind AS callee_kind, callee.file_path AS callee_file, \
                   callee.line AS callee_line";

        let inst_rows = graph
            .execute_read(tenant_id, inst_cypher, &[("fqn", source_fqn), ("repo_id", repo_str)])
            .await?;

        for row in inst_rows {
            let Some(callee_fqn) = row.get::<String>("callee_fqn").ok() else { continue };
            if visited.contains(&callee_fqn) {
                *cycles_detected = true;
                continue;
            }
            edges.push(RawEdge {
                from_fqn: source_fqn.clone(),
                to_fqn: callee_fqn.clone(),
                depth,
                provenance: EdgeProvenance::Monomorph,
                from_node: TraversalNode {
                    fqn: source_fqn.clone(),
                    name: None,
                    kind: None,
                    file_path: None,
                    line: None,
                },
            });
        }
    }

    Ok(edges)
}

// ---------------------------------------------------------------------------
// Root node lookup
// ---------------------------------------------------------------------------

pub(super) async fn fetch_root_node(
    graph: &TenantGraph,
    tenant_id: &TenantId,
    repo_str: &str,
    fqn: &str,
) -> Result<Option<TraversalNode>, QueryError> {
    let cypher = "\
        MATCH (n:Item {fqn: $fqn, repo_id: $repo_id}) \
        RETURN n.fqn AS fqn, n.name AS name, n.kind AS kind, \
               n.file_path AS file_path, n.line AS line \
        LIMIT 1";
    let rows = graph.execute_read(tenant_id, cypher, &[("fqn", fqn), ("repo_id", repo_str)]).await?;
    let Some(row) = rows.into_iter().next() else { return Ok(None) };
    let Some(node_fqn) = row.get::<String>("fqn").ok() else { return Ok(None) };
    Ok(Some(TraversalNode {
        fqn: node_fqn,
        name: row.get::<String>("name").ok(),
        kind: row.get::<String>("kind").ok(),
        file_path: row.get::<String>("file_path").ok(),
        line: row.get::<i64>("line").ok(),
    }))
}
