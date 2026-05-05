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

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

use rb_schemas::TenantId;
use rb_storage_neo4j::TenantGraph;
use serde::Serialize;
use uuid::Uuid;

use crate::QueryError;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const DEFAULT_DEPTH: u32 = 3;
pub const MAX_DEPTH: u32 = 10;
pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 200;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Per-edge dispatch provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeProvenance {
    /// Static direct call — `CALLS` edge, non-dynamic dispatch.
    Direct,
    /// Monomorphized generic instantiation — `CALL_INSTANTIATES` edge.
    Monomorph,
    /// Dynamic dispatch candidate — `CALLS` edge with `dispatch = "dynamic"`.
    DynCandidate,
}

/// A node discovered during BFS traversal.
#[derive(Debug, Clone, Serialize)]
pub struct TraversalNode {
    pub fqn: String,
    pub name: Option<String>,
    pub kind: Option<String>,
    pub file_path: Option<String>,
    pub line: Option<i64>,
}

/// A directed edge discovered during BFS traversal.
#[derive(Debug, Clone, Serialize)]
pub struct TraversalEdge {
    /// FQN of the node closer to the root in traversal direction.
    pub from_fqn: String,
    /// FQN of the node further from the root.
    pub to_fqn: String,
    /// BFS depth at which this edge was first discovered (1-indexed from root).
    pub depth: u32,
    pub provenance: EdgeProvenance,
}

/// Result of a BFS traversal.
#[derive(Debug, Serialize)]
pub struct TraversalResult {
    /// The root node from which traversal started.
    pub root: TraversalNode,
    /// All unique nodes encountered during traversal (excluding root), in BFS order.
    pub nodes: Vec<TraversalNode>,
    /// All edges traversed in BFS order, after pagination slicing.
    pub edges: Vec<TraversalEdge>,
    /// `true` when a cycle was detected (a previously visited node was encountered again).
    pub cycles_detected: bool,
    /// Opaque cursor for the next page; absent on the last page.
    pub next_cursor: Option<String>,
}

/// Pagination + depth controls for BFS traversal.
pub struct TraversalOptions {
    /// BFS depth limit (1–[`MAX_DEPTH`]).
    pub depth: u32,
    /// Max edges per page (1–[`MAX_LIMIT`]).
    pub limit: usize,
    /// Byte offset encoded in the cursor — number of edges to skip.
    pub offset: usize,
}

impl Default for TraversalOptions {
    fn default() -> Self {
        Self { depth: DEFAULT_DEPTH, limit: DEFAULT_LIMIT, offset: 0 }
    }
}

// ---------------------------------------------------------------------------
// Internal edge accumulator (before pagination)
// ---------------------------------------------------------------------------

struct RawEdge {
    from_fqn: String,
    to_fqn: String,
    depth: u32,
    provenance: EdgeProvenance,
    /// Source node info (the "from" side).
    from_node: TraversalNode,
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
async fn one_hop_callers(
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
async fn one_hop_callees(
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
// Cursor encoding/decoding
// ---------------------------------------------------------------------------

fn encode_cursor(offset: usize) -> String {
    let mut s = String::new();
    let _ = write!(s, "{offset}");
    URL_SAFE_NO_PAD.encode(s.as_bytes())
}

#[cfg(test)]
fn decode_cursor(cursor: &str) -> Option<usize> {
    let bytes = URL_SAFE_NO_PAD.decode(cursor.as_bytes()).ok()?;
    let s = String::from_utf8(bytes).ok()?;
    s.parse::<usize>().ok()
}

// ---------------------------------------------------------------------------
// Root node lookup
// ---------------------------------------------------------------------------

async fn fetch_root_node(
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
    let repo_str = repo_id.to_string();

    let root = match fetch_root_node(graph, tenant_id, &repo_str, fqn).await? {
        Some(n) => n,
        None => TraversalNode { fqn: fqn.to_owned(), name: None, kind: None, file_path: None, line: None },
    };

    let mut all_edges: Vec<RawEdge> = Vec::new();
    let mut node_map: HashMap<String, TraversalNode> = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut cycles_detected = false;

    visited.insert(fqn.to_owned());
    let mut frontier = vec![fqn.to_owned()];

    for depth in 1..=opts.depth {
        if frontier.is_empty() {
            break;
        }
        let hop_edges = one_hop_callers(
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

    Ok(paginate(root, &all_edges, &node_map, &opts, cycles_detected))
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
    let repo_str = repo_id.to_string();

    let root = match fetch_root_node(graph, tenant_id, &repo_str, fqn).await? {
        Some(n) => n,
        None => TraversalNode { fqn: fqn.to_owned(), name: None, kind: None, file_path: None, line: None },
    };

    let mut all_edges: Vec<RawEdge> = Vec::new();
    let mut node_map: HashMap<String, TraversalNode> = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut cycles_detected = false;

    visited.insert(fqn.to_owned());
    let mut frontier = vec![fqn.to_owned()];

    for depth in 1..=opts.depth {
        if frontier.is_empty() {
            break;
        }
        // For callees, we need callee node info. Fetch it via a separate look-up
        // after collecting edge targets.
        let hop_edges = one_hop_callees(
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
                // Look up callee node info.
                if let Ok(Some(node)) =
                    fetch_root_node(graph, tenant_id, &repo_str, &callee_fqn).await
                {
                    node_map.entry(callee_fqn).or_insert(node);
                }
            }
            // Populate from_node with source info (already in visited, use root or prior).
            if let Some(n) = node_map.get(&edge.from_fqn) {
                edge.from_node = n.clone();
            } else if edge.from_fqn == fqn {
                edge.from_node = root.clone();
            }
            all_edges.push(edge);
        }
        frontier = next_frontier;
    }

    Ok(paginate(root, &all_edges, &node_map, &opts, cycles_detected))
}

// ---------------------------------------------------------------------------
// Pagination helper
// ---------------------------------------------------------------------------

fn paginate(
    root: TraversalNode,
    all_edges: &[RawEdge],
    node_map: &HashMap<String, TraversalNode>,
    opts: &TraversalOptions,
    cycles_detected: bool,
) -> TraversalResult {
    let total = all_edges.len();
    let start = opts.offset.min(total);
    let end = (start + opts.limit).min(total);

    let page_edges: Vec<TraversalEdge> = all_edges[start..end]
        .iter()
        .map(|e| TraversalEdge {
            from_fqn: e.from_fqn.clone(),
            to_fqn: e.to_fqn.clone(),
            depth: e.depth,
            provenance: e.provenance.clone(),
        })
        .collect();

    let next_cursor = if end < total { Some(encode_cursor(end)) } else { None };

    // Collect unique nodes referenced by this page's edges.
    let mut seen_fqns: HashSet<&str> = HashSet::new();
    let mut nodes: Vec<TraversalNode> = Vec::new();
    for edge in &page_edges {
        for fqn in [edge.from_fqn.as_str(), edge.to_fqn.as_str()] {
            if fqn != root.fqn && seen_fqns.insert(fqn) {
                if let Some(n) = node_map.get(fqn) {
                    nodes.push(n.clone());
                } else {
                    nodes.push(TraversalNode {
                        fqn: fqn.to_owned(),
                        name: None,
                        kind: None,
                        file_path: None,
                        line: None,
                    });
                }
            }
        }
    }

    TraversalResult { root, nodes, edges: page_edges, cycles_detected, next_cursor }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_roundtrip() {
        for offset in [0usize, 1, 50, 199, 10_000] {
            let encoded = encode_cursor(offset);
            assert_eq!(decode_cursor(&encoded), Some(offset));
        }
    }

    #[test]
    fn cursor_decode_rejects_garbage() {
        assert_eq!(decode_cursor("!!!not_base64!!!"), None);
    }

    #[test]
    fn traversal_options_default() {
        let opts = TraversalOptions::default();
        assert_eq!(opts.depth, DEFAULT_DEPTH);
        assert_eq!(opts.limit, DEFAULT_LIMIT);
        assert_eq!(opts.offset, 0);
    }

    #[test]
    fn paginate_empty() {
        let root = TraversalNode { fqn: "root".into(), name: None, kind: None, file_path: None, line: None };
        let result = paginate(
            root,
            &[],
            &HashMap::new(),
            &TraversalOptions::default(),
            false,
        );
        assert!(result.edges.is_empty());
        assert!(result.nodes.is_empty());
        assert!(!result.cycles_detected);
        assert!(result.next_cursor.is_none());
    }

    #[test]
    fn paginate_produces_next_cursor_when_more_pages_remain() {
        let root = TraversalNode { fqn: "root".into(), name: None, kind: None, file_path: None, line: None };
        let edges: Vec<RawEdge> = (0..10)
            .map(|i| RawEdge {
                from_fqn: format!("caller_{i}"),
                to_fqn: "root".into(),
                depth: 1,
                provenance: EdgeProvenance::Direct,
                from_node: TraversalNode {
                    fqn: format!("caller_{i}"),
                    name: None,
                    kind: None,
                    file_path: None,
                    line: None,
                },
            })
            .collect();
        let result = paginate(
            root,
            &edges,
            &HashMap::new(),
            &TraversalOptions { depth: DEFAULT_DEPTH, limit: 3, offset: 0 },
            false,
        );
        assert_eq!(result.edges.len(), 3);
        assert!(result.next_cursor.is_some());
        // Cursor should decode to offset 3.
        assert_eq!(decode_cursor(result.next_cursor.as_deref().unwrap()), Some(3));
    }

    #[test]
    fn paginate_last_page_has_no_cursor() {
        let root = TraversalNode { fqn: "root".into(), name: None, kind: None, file_path: None, line: None };
        let edges: Vec<RawEdge> = (0..5)
            .map(|i| RawEdge {
                from_fqn: format!("caller_{i}"),
                to_fqn: "root".into(),
                depth: 1,
                provenance: EdgeProvenance::Direct,
                from_node: TraversalNode {
                    fqn: format!("caller_{i}"),
                    name: None,
                    kind: None,
                    file_path: None,
                    line: None,
                },
            })
            .collect();
        let result = paginate(
            root,
            &edges,
            &HashMap::new(),
            &TraversalOptions { depth: DEFAULT_DEPTH, limit: 10, offset: 0 },
            false,
        );
        assert_eq!(result.edges.len(), 5);
        assert!(result.next_cursor.is_none());
    }

    #[test]
    fn edge_provenance_serializes_correctly() {
        assert_eq!(
            serde_json::to_string(&EdgeProvenance::Direct).unwrap(),
            "\"direct\""
        );
        assert_eq!(
            serde_json::to_string(&EdgeProvenance::Monomorph).unwrap(),
            "\"monomorph\""
        );
        assert_eq!(
            serde_json::to_string(&EdgeProvenance::DynCandidate).unwrap(),
            "\"dyn_candidate\""
        );
    }
}
