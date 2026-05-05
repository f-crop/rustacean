//! Module tree query and in-memory tree construction (REQ-DP-06).
//!
//! Single SQL: `SELECT fqn, kind, source_path, line_start, line_end FROM
//! {tenant}.code_symbols WHERE repo_id = $1 ORDER BY fqn`.
//! Tree built in-Rust by splitting on `::`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;
use serde::Serialize;
use uuid::Uuid;

use crate::error::QueryError;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A node in the crate/module hierarchy tree.
///
/// Leaf nodes (functions, structs, etc.) have `children` empty and may carry
/// `source_path`, `line_start`, `line_end`. Interior `MOD` nodes that are not
/// explicitly listed in `code_symbols` are synthesised from the FQN path and
/// have `None` for all source fields.
#[derive(Debug, Clone, Serialize)]
pub struct ModuleNode {
    pub name: String,
    pub fqn: String,
    pub kind: String,
    pub source_path: Option<String>,
    pub line_start: Option<i32>,
    pub line_end: Option<i32>,
    pub children: Vec<ModuleNode>,
}

/// In-process 60-second cache keyed by `(repo_id, last_ingest_run_id)`.
///
/// `last_ingest_run_id` is `Uuid::nil()` when no succeeded run exists yet,
/// so cold repos still benefit from the cache after the first build.
pub type ModuleTreeCache = Cache<(Uuid, Uuid), Arc<ModuleNode>>;

/// Construct a fresh [`ModuleTreeCache`] with a 60-second TTL and a capacity
/// cap of 1 000 entries (enough for hundreds of repos).
#[must_use]
pub fn new_module_tree_cache() -> ModuleTreeCache {
    Cache::builder()
        .max_capacity(1_000)
        .time_to_live(Duration::from_secs(60))
        .build()
}

// ---------------------------------------------------------------------------
// Public query function
// ---------------------------------------------------------------------------

/// Fetch all symbols for `repo_id` from the tenant schema and build the
/// module/item tree in-process.
///
/// Issues exactly one SQL query per ADR-008 §3.6 / AC2.
///
/// # Errors
///
/// Returns [`QueryError::Sqlx`] on database failure.
pub async fn fetch_module_tree(
    pool: &sqlx::PgPool,
    tenant_ctx: &rb_tenant::TenantCtx,
    repo_id: Uuid,
) -> Result<ModuleNode, QueryError> {
    let table = tenant_ctx.qualify("code_symbols");

    // One SQL query — ADR-008 AC2.
    let rows: Vec<SymbolRow> =
        sqlx::query_as(&format!(
            "SELECT fqn, kind, source_path, line_start, line_end \
             FROM {table} \
             WHERE repo_id = $1 \
             ORDER BY fqn"
        ))
        .bind(repo_id)
        .fetch_all(pool)
        .await?;

    tracing::debug!(
        %repo_id,
        row_count = rows.len(),
        "building module tree from code_symbols"
    );

    Ok(build_tree(rows))
}

// ---------------------------------------------------------------------------
// Tree construction (pure, no I/O)
// ---------------------------------------------------------------------------

type SymbolRow = (String, String, Option<String>, Option<i32>, Option<i32>);

struct NodeInfo {
    kind: String,
    source_path: Option<String>,
    line_start: Option<i32>,
    line_end: Option<i32>,
}

fn build_tree(rows: Vec<SymbolRow>) -> ModuleNode {
    if rows.is_empty() {
        return ModuleNode {
            name: "(empty)".to_owned(),
            fqn: String::new(),
            kind: "MOD".to_owned(),
            source_path: None,
            line_start: None,
            line_end: None,
            children: Vec::new(),
        };
    }

    // 1. Populate the node map with explicit rows.
    let mut node_map: HashMap<String, NodeInfo> = HashMap::with_capacity(rows.len() * 2);
    for (fqn, kind, source_path, line_start, line_end) in rows {
        // Ensure all ancestor segments exist as synthetic MOD nodes.
        let parts: Vec<&str> = fqn.split("::").collect();
        for depth in 1..parts.len() {
            let ancestor = parts[..depth].join("::");
            node_map.entry(ancestor).or_insert_with(|| NodeInfo {
                kind: "MOD".to_owned(),
                source_path: None,
                line_start: None,
                line_end: None,
            });
        }
        // The explicit row wins (overwrites a synthetic placeholder if present).
        node_map.insert(fqn, NodeInfo { kind, source_path, line_start, line_end });
    }

    // 2. Build the parent → children map.
    let mut children_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut roots: Vec<String> = Vec::new();

    for fqn in node_map.keys() {
        if let Some(sep) = fqn.rfind("::") {
            let parent = fqn[..sep].to_owned();
            children_map.entry(parent).or_default().push(fqn.clone());
        } else {
            roots.push(fqn.clone());
        }
    }

    // Sort for deterministic output.
    roots.sort_unstable();
    for kids in children_map.values_mut() {
        kids.sort_unstable();
    }

    // 3. Build the tree recursively.
    if roots.len() == 1 {
        make_node(&roots[0], &node_map, &children_map)
    } else {
        // Workspace with multiple crate roots — wrap in a virtual root.
        let children = roots
            .iter()
            .map(|r| make_node(r, &node_map, &children_map))
            .collect();
        ModuleNode {
            name: "(workspace)".to_owned(),
            fqn: String::new(),
            kind: "MOD".to_owned(),
            source_path: None,
            line_start: None,
            line_end: None,
            children,
        }
    }
}

fn make_node(
    fqn: &str,
    node_map: &HashMap<String, NodeInfo>,
    children_map: &HashMap<String, Vec<String>>,
) -> ModuleNode {
    let info = &node_map[fqn];
    let name = fqn.rsplit("::").next().unwrap_or(fqn).to_owned();

    let children = children_map
        .get(fqn)
        .map(|kids| kids.iter().map(|c| make_node(c, node_map, children_map)).collect())
        .unwrap_or_default();

    ModuleNode {
        name,
        fqn: fqn.to_owned(),
        kind: info.kind.clone(),
        source_path: info.source_path.clone(),
        line_start: info.line_start,
        line_end: info.line_end,
        children,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn row(fqn: &str, kind: &str) -> SymbolRow {
        (fqn.to_owned(), kind.to_owned(), None, None, None)
    }

    fn row_with_source(fqn: &str, kind: &str, path: &str, start: i32, end: i32) -> SymbolRow {
        (fqn.to_owned(), kind.to_owned(), Some(path.to_owned()), Some(start), Some(end))
    }

    #[test]
    fn empty_rows_returns_empty_node() {
        let tree = build_tree(vec![]);
        assert_eq!(tree.name, "(empty)");
        assert_eq!(tree.kind, "MOD");
        assert!(tree.children.is_empty());
    }

    #[test]
    fn single_root_crate() {
        let rows = vec![
            row("mycrate", "MOD"),
            row("mycrate::lib", "MOD"),
            row_with_source("mycrate::lib::Foo", "STRUCT", "src/lib.rs", 1, 10),
        ];
        let tree = build_tree(rows);
        assert_eq!(tree.name, "mycrate");
        assert_eq!(tree.fqn, "mycrate");
        assert_eq!(tree.kind, "MOD");
        assert_eq!(tree.children.len(), 1);

        let lib = &tree.children[0];
        assert_eq!(lib.name, "lib");
        assert_eq!(lib.fqn, "mycrate::lib");
        assert_eq!(lib.children.len(), 1);

        let foo = &lib.children[0];
        assert_eq!(foo.name, "Foo");
        assert_eq!(foo.kind, "STRUCT");
        assert_eq!(foo.source_path.as_deref(), Some("src/lib.rs"));
        assert_eq!(foo.line_start, Some(1));
        assert_eq!(foo.line_end, Some(10));
    }

    #[test]
    fn synthetic_mod_nodes_created_for_missing_parents() {
        // Only the leaf is explicit; intermediate MOD nodes are synthesised.
        let rows = vec![row_with_source(
            "alloc::vec::Vec::push",
            "FN",
            "src/vec.rs",
            42,
            50,
        )];
        let tree = build_tree(rows);
        assert_eq!(tree.name, "alloc");
        let vec_mod = &tree.children[0];
        assert_eq!(vec_mod.name, "vec");
        assert_eq!(vec_mod.kind, "MOD"); // synthesised
        let vec_struct = &vec_mod.children[0];
        assert_eq!(vec_struct.name, "Vec");
        assert_eq!(vec_struct.kind, "MOD"); // synthesised
        let push = &vec_struct.children[0];
        assert_eq!(push.name, "push");
        assert_eq!(push.kind, "FN");
        assert_eq!(push.source_path.as_deref(), Some("src/vec.rs"));
    }

    #[test]
    fn explicit_row_overrides_synthetic_mod() {
        let rows = vec![
            row("alloc::vec", "MOD"),
            row_with_source("alloc::vec::Vec", "STRUCT", "src/vec.rs", 1, 100),
        ];
        let tree = build_tree(rows);
        let vec_mod = &tree.children[0];
        assert_eq!(vec_mod.kind, "MOD"); // explicit, not synthetic
        let vec_struct = &vec_mod.children[0];
        assert_eq!(vec_struct.kind, "STRUCT");
    }

    #[test]
    fn multiple_roots_wrapped_in_workspace_node() {
        let rows = vec![row("crate_a", "MOD"), row("crate_b", "MOD")];
        let tree = build_tree(rows);
        assert_eq!(tree.name, "(workspace)");
        assert_eq!(tree.children.len(), 2);
        // Children are sorted alphabetically.
        assert_eq!(tree.children[0].name, "crate_a");
        assert_eq!(tree.children[1].name, "crate_b");
    }

    #[test]
    fn children_are_sorted_alphabetically() {
        let rows = vec![
            row("crate::z_module", "MOD"),
            row("crate::a_module", "MOD"),
            row("crate::m_module", "MOD"),
        ];
        let tree = build_tree(rows);
        let names: Vec<&str> = tree.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["a_module", "m_module", "z_module"]);
    }

    #[test]
    fn fqn_stored_correctly_on_all_nodes() {
        let rows = vec![row_with_source("a::b::c", "FN", "src/c.rs", 1, 5)];
        let tree = build_tree(rows);
        assert_eq!(tree.fqn, "a");
        assert_eq!(tree.children[0].fqn, "a::b");
        assert_eq!(tree.children[0].children[0].fqn, "a::b::c");
    }
}
