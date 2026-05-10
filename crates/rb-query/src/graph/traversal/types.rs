use serde::Serialize;

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
        Self {
            depth: DEFAULT_DEPTH,
            limit: DEFAULT_LIMIT,
            offset: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_options_default() {
        let opts = TraversalOptions::default();
        assert_eq!(opts.depth, DEFAULT_DEPTH);
        assert_eq!(opts.limit, DEFAULT_LIMIT);
        assert_eq!(opts.offset, 0);
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
