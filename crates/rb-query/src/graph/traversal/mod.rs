//! Caller/callee BFS traversal over the Neo4j call graph (REQ-DP-03).
//!
//! Traverses `CALLS` and `CALL_INSTANTIATES` edges tenant-isolated via
//! [`TenantGraph::execute_read`] (ADR-007 §3.4 / ADR-008 §3.3).
//!
//! BFS is implemented in Rust with one Neo4j round-trip per depth level.
//! Per-edge provenance is derived from the relationship type and `dispatch`
//! property:
//!   - `direct`        — `CALLS` edge without `dispatch = "dynamic"`
//!   - `monomorph`     — `CALL_INSTANTIATES` edge
//!   - `dyn_candidate` — `CALLS` edge with `dispatch = "dynamic"`

mod bfs;
mod pagination;
mod types;

pub use types::{
    DEFAULT_DEPTH, DEFAULT_LIMIT, MAX_DEPTH, MAX_LIMIT,
    EdgeProvenance, TraversalEdge, TraversalNode, TraversalOptions, TraversalResult,
};

use std::collections::HashMap;

use rb_schemas::TenantId;
use rb_storage_neo4j::TenantGraph;
use uuid::Uuid;

use crate::QueryError;

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Fetch all callers of `fqn` within `repo_id`, up to `opts.depth` BFS hops.
///
/// Returns up to `opts.limit` edges, starting at `opts.offset` (pagination).
/// Sets `cycles_detected = true` when a previously-visited node is re-encountered.
///
/// # Errors
///
/// Propagates [`QueryError::Graph`] on driver or injection failure.
pub async fn fetch_callers(
    graph: &TenantGraph,
    tenant_id: &TenantId,
    repo_id: Uuid,
    fqn: &str,
    opts: TraversalOptions,
) -> Result<TraversalResult, QueryError> {
    use std::collections::HashSet;

    let repo_str = repo_id.to_string();

    let root = match bfs::fetch_root_node(graph, tenant_id, &repo_str, fqn).await? {
        Some(n) => n,
        None => TraversalNode { fqn: fqn.to_owned(), name: None, kind: None, file_path: None, line: None },
    };

    let mut all_edges = Vec::new();
    let mut node_map: HashMap<String, TraversalNode> = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut cycles_detected = false;

    visited.insert(fqn.to_owned());
    let mut frontier = vec![fqn.to_owned()];

    for depth in 1..=opts.depth {
        if frontier.is_empty() {
            break;
        }
        let hop_edges = bfs::one_hop_callers(
            graph,
            tenant_id,
            &repo_str,
            &frontier,
            depth,
            &visited,
            &mut cycles_detected,
        )
        .await?;

        let mut next_frontier = Vec::new();
        for edge in hop_edges {
            let caller_fqn = edge.from_fqn.clone();
            if !visited.contains(&caller_fqn) {
                visited.insert(caller_fqn.clone());
                next_frontier.push(caller_fqn.clone());
                node_map.entry(caller_fqn).or_insert_with(|| edge.from_node.clone());
            }
            all_edges.push(edge);
        }
        frontier = next_frontier;
    }

    Ok(pagination::paginate(root, &all_edges, &node_map, &opts, cycles_detected))
}

/// Fetch all callees of `fqn` within `repo_id`, up to `opts.depth` BFS hops.
///
/// # Errors
///
/// Propagates [`QueryError::Graph`] on driver or injection failure.
pub async fn fetch_callees(
    graph: &TenantGraph,
    tenant_id: &TenantId,
    repo_id: Uuid,
    fqn: &str,
    opts: TraversalOptions,
) -> Result<TraversalResult, QueryError> {
    use std::collections::HashSet;

    let repo_str = repo_id.to_string();

    let root = match bfs::fetch_root_node(graph, tenant_id, &repo_str, fqn).await? {
        Some(n) => n,
        None => TraversalNode { fqn: fqn.to_owned(), name: None, kind: None, file_path: None, line: None },
    };

    let mut all_edges = Vec::new();
    let mut node_map: HashMap<String, TraversalNode> = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut cycles_detected = false;

    visited.insert(fqn.to_owned());
    let mut frontier = vec![fqn.to_owned()];

    for depth in 1..=opts.depth {
        if frontier.is_empty() {
            break;
        }
        let hop_edges = bfs::one_hop_callees(
            graph,
            tenant_id,
            &repo_str,
            &frontier,
            depth,
            &visited,
            &mut cycles_detected,
        )
        .await?;

        let mut next_frontier = Vec::new();
        for mut edge in hop_edges {
            let callee_fqn = edge.to_fqn.clone();
            if !visited.contains(&callee_fqn) {
                visited.insert(callee_fqn.clone());
                next_frontier.push(callee_fqn.clone());
                if let Ok(Some(node)) =
                    bfs::fetch_root_node(graph, tenant_id, &repo_str, &callee_fqn).await
                {
                    node_map.entry(callee_fqn).or_insert(node);
                }
            }
            if let Some(n) = node_map.get(&edge.from_fqn) {
                edge.from_node = n.clone();
            } else if edge.from_fqn == fqn {
                edge.from_node = root.clone();
            }
            all_edges.push(edge);
        }
        frontier = next_frontier;
    }

    Ok(pagination::paginate(root, &all_edges, &node_map, &opts, cycles_detected))
}
