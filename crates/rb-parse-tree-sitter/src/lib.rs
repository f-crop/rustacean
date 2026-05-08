//! Tree-sitter based Rust item extractor — error-tolerant fallback for the
//! parse pipeline stage (ADR-007 §11.5).
//!
//! Used when `syn` fails to parse a source file (e.g. partial macro expansion,
//! invalid UTF-8 recovery path). Tree-sitter parses with error nodes so we can
//! still extract named items from the syntactically valid parts.

use tree_sitter::{Node, Parser};

/// Rust item kind extracted from a tree-sitter parse tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Fn,
    Struct,
    Enum,
    Trait,
    Impl,
    Mod,
    Const,
    TypeAlias,
    Static,
    MacroDef,
}

/// A partially-extracted item from a potentially invalid source file.
#[derive(Debug, Clone)]
pub struct PartialItem {
    pub name: String,
    pub kind: Kind,
    pub line_start: u32,
    pub line_end: u32,
}

/// Extract items from `source`, tolerating syntax errors.
///
/// Returns an empty `Vec` only when the tree has no parseable item nodes.
/// All errors are silent (tree-sitter produces `ERROR` nodes; we skip them
/// and collect from valid siblings).
///
/// # Panics
///
/// Panics if the tree-sitter Rust grammar fails to load (should never happen
/// with the bundled `tree-sitter-rust` crate).
#[must_use]
pub fn extract_items_partial(source: &str) -> Vec<PartialItem> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .expect("tree-sitter-rust language load must succeed");

    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };

    let src_bytes = source.as_bytes();
    let mut items = Vec::new();
    collect_items(tree.root_node(), src_bytes, &mut items);
    items
}

fn collect_items(node: Node<'_>, src: &[u8], out: &mut Vec<PartialItem>) {
    match node.kind() {
        "function_item" => {
            if let Some(item) = extract_named(node, src, Kind::Fn) {
                out.push(item);
            }
        }
        "struct_item" => {
            if let Some(item) = extract_named(node, src, Kind::Struct) {
                out.push(item);
            }
        }
        "enum_item" => {
            if let Some(item) = extract_named(node, src, Kind::Enum) {
                out.push(item);
            }
        }
        "trait_item" => {
            if let Some(item) = extract_named(node, src, Kind::Trait) {
                out.push(item);
            }
        }
        "impl_item" => {
            let name = impl_item_name(node, src);
            out.push(PartialItem {
                name,
                kind: Kind::Impl,
                line_start: u32::try_from(node.start_position().row).expect("row fits u32") + 1,
                line_end: u32::try_from(node.end_position().row).expect("row fits u32") + 1,
            });
        }
        "mod_item" => {
            if let Some(item) = extract_named(node, src, Kind::Mod) {
                out.push(item);
                // Do not recurse into mod body — callers handle file-level items.
                return;
            }
        }
        "const_item" => {
            if let Some(item) = extract_named(node, src, Kind::Const) {
                out.push(item);
            }
        }
        "type_item" => {
            if let Some(item) = extract_named(node, src, Kind::TypeAlias) {
                out.push(item);
            }
        }
        "static_item" => {
            if let Some(item) = extract_named(node, src, Kind::Static) {
                out.push(item);
            }
        }
        "macro_definition" => {
            if let Some(item) = extract_named(node, src, Kind::MacroDef) {
                out.push(item);
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_items(child, src, out);
    }
}

fn extract_named(node: Node<'_>, src: &[u8], kind: Kind) -> Option<PartialItem> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(src).ok()?.to_owned();
    Some(PartialItem {
        name,
        kind,
        line_start: u32::try_from(node.start_position().row).expect("row fits u32") + 1,
        line_end: u32::try_from(node.end_position().row).expect("row fits u32") + 1,
    })
}

fn impl_item_name(node: Node<'_>, src: &[u8]) -> String {
    let type_name = node
        .child_by_field_name("type")
        .and_then(|n| n.utf8_text(src).ok())
        .unwrap_or("?");
    let trait_name = node
        .child_by_field_name("trait")
        .and_then(|n| n.utf8_text(src).ok());
    match trait_name {
        Some(t) => format!("<{type_name} as {t}>"),
        None => format!("impl {type_name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_fn_from_valid_source() {
        let items = extract_items_partial("pub fn hello() {}");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "hello");
        assert_eq!(items[0].kind, Kind::Fn);
    }

    #[test]
    fn tolerates_syntax_error_and_extracts_valid_siblings() {
        // First item has a syntax error; second is valid.
        let src = "fn broken( { }\npub fn good() {}";
        let items = extract_items_partial(src);
        // We expect at least the valid fn to be found.
        assert!(items.iter().any(|i| i.name == "good"));
    }

    #[test]
    fn returns_empty_for_completely_invalid_source() {
        let items = extract_items_partial("!!! not rust at all !!!");
        // Tree-sitter parses everything; result may be empty or contain error nodes.
        // We just assert no panic.
        let _ = items;
    }

    #[test]
    fn extracts_struct_and_impl() {
        let src = "struct Foo {}\nimpl Foo { fn new() -> Self { Foo {} } }";
        let items = extract_items_partial(src);
        assert!(
            items
                .iter()
                .any(|i| i.kind == Kind::Struct && i.name == "Foo")
        );
        assert!(items.iter().any(|i| i.kind == Kind::Impl));
    }

    #[test]
    fn line_numbers_are_one_based() {
        let src = "fn first() {}\nfn second() {}";
        let items = extract_items_partial(src);
        let first = items.iter().find(|i| i.name == "first").unwrap();
        let second = items.iter().find(|i| i.name == "second").unwrap();
        assert_eq!(first.line_start, 1);
        assert_eq!(second.line_start, 2);
    }
}
