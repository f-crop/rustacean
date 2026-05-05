use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

use super::bfs::RawEdge;
use super::types::{TraversalEdge, TraversalNode, TraversalResult};

// ---------------------------------------------------------------------------
// Cursor encoding/decoding
// ---------------------------------------------------------------------------

pub(super) fn encode_cursor(offset: usize) -> String {
    let mut s = String::new();
    let _ = write!(s, "{offset}");
    URL_SAFE_NO_PAD.encode(s.as_bytes())
}

#[cfg(test)]
pub(super) fn decode_cursor(cursor: &str) -> Option<usize> {
    let bytes = URL_SAFE_NO_PAD.decode(cursor.as_bytes()).ok()?;
    let s = String::from_utf8(bytes).ok()?;
    s.parse::<usize>().ok()
}

// ---------------------------------------------------------------------------
// Pagination helper
// ---------------------------------------------------------------------------

pub(super) fn paginate(
    root: TraversalNode,
    all_edges: &[RawEdge],
    node_map: &HashMap<String, TraversalNode>,
    opts: &super::types::TraversalOptions,
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
    use std::collections::HashMap;

    use super::super::types::{EdgeProvenance, TraversalOptions};
    use super::super::bfs::RawEdge;
    use super::super::types::TraversalNode;
    use super::*;

    fn make_node(fqn: &str) -> TraversalNode {
        TraversalNode { fqn: fqn.into(), name: None, kind: None, file_path: None, line: None }
    }

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
    fn paginate_empty() {
        let result = paginate(
            make_node("root"),
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
        let edges: Vec<RawEdge> = (0..10)
            .map(|i| RawEdge {
                from_fqn: format!("caller_{i}"),
                to_fqn: "root".into(),
                depth: 1,
                provenance: EdgeProvenance::Direct,
                from_node: make_node(&format!("caller_{i}")),
            })
            .collect();
        let result = paginate(
            make_node("root"),
            &edges,
            &HashMap::new(),
            &TraversalOptions { depth: 3, limit: 3, offset: 0 },
            false,
        );
        assert_eq!(result.edges.len(), 3);
        assert!(result.next_cursor.is_some());
        assert_eq!(decode_cursor(result.next_cursor.as_deref().unwrap()), Some(3));
    }

    #[test]
    fn paginate_last_page_has_no_cursor() {
        let edges: Vec<RawEdge> = (0..5)
            .map(|i| RawEdge {
                from_fqn: format!("caller_{i}"),
                to_fqn: "root".into(),
                depth: 1,
                provenance: EdgeProvenance::Direct,
                from_node: make_node(&format!("caller_{i}")),
            })
            .collect();
        let result = paginate(
            make_node("root"),
            &edges,
            &HashMap::new(),
            &TraversalOptions { depth: 3, limit: 10, offset: 0 },
            false,
        );
        assert_eq!(result.edges.len(), 5);
        assert!(result.next_cursor.is_none());
    }
}
