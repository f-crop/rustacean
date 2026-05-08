//! Syn-based Rust item extractor for the parse pipeline stage.
//!
//! Parses syntactically valid `*.rs` source via `syn` and extracts top-level
//! items with their kind, name, and source location (ADR-007 §11.5).

use proc_macro2::LineColumn;
use syn::spanned::Spanned as _;
use syn::visit::Visit;

#[derive(Debug, thiserror::Error)]
pub enum ParseSynError {
    #[error("syn parse error: {0}")]
    Syn(#[from] syn::Error),
}

/// Rust item kind — mirrors `ItemKind` in `pipeline.proto` (ADR-007 §3.4).
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

/// A single extracted Rust item.
#[derive(Debug, Clone)]
pub struct ExtractedItem {
    /// Simple ident name (e.g. `"my_fn"`).
    pub name: String,
    pub kind: Kind,
    pub line_start: u32,
    pub line_end: u32,
}

/// Extract all top-level items from `source`.
///
/// Returns `Err` only when the source cannot be parsed at all. Individual
/// items that produce no name (rare) are silently skipped.
///
/// # Errors
///
/// Returns [`ParseSynError::Syn`] if `source` cannot be parsed as a Rust file.
pub fn extract_items(source: &str) -> Result<Vec<ExtractedItem>, ParseSynError> {
    let file: syn::File = syn::parse_str(source)?;
    let mut visitor = ItemVisitor { items: Vec::new() };
    visitor.visit_file(&file);
    Ok(visitor.items)
}

struct ItemVisitor {
    items: Vec<ExtractedItem>,
}

impl ItemVisitor {
    fn push(&mut self, name: String, kind: Kind, span: proc_macro2::Span) {
        let start: LineColumn = span.start();
        let end: LineColumn = span.end();
        self.items.push(ExtractedItem {
            name,
            kind,
            line_start: u32::try_from(start.line).expect("line number fits u32"),
            line_end: u32::try_from(end.line).expect("line number fits u32"),
        });
    }
}

impl<'ast> Visit<'ast> for ItemVisitor {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.push(node.sig.ident.to_string(), Kind::Fn, node.span());
    }

    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        self.push(node.ident.to_string(), Kind::Struct, node.span());
    }

    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        self.push(node.ident.to_string(), Kind::Enum, node.span());
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        self.push(node.ident.to_string(), Kind::Trait, node.span());
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let name = impl_name(node);
        self.push(name, Kind::Impl, node.span());
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        self.push(node.ident.to_string(), Kind::Mod, node.span());
        // Do NOT recurse into inline mod bodies — callers handle nested files.
    }

    fn visit_item_const(&mut self, node: &'ast syn::ItemConst) {
        self.push(node.ident.to_string(), Kind::Const, node.span());
    }

    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        self.push(node.ident.to_string(), Kind::TypeAlias, node.span());
    }

    fn visit_item_static(&mut self, node: &'ast syn::ItemStatic) {
        self.push(node.ident.to_string(), Kind::Static, node.span());
    }

    fn visit_item_macro(&mut self, node: &'ast syn::ItemMacro) {
        if let Some(ident) = &node.ident {
            self.push(ident.to_string(), Kind::MacroDef, node.span());
        }
    }
}

fn impl_name(node: &syn::ItemImpl) -> String {
    let self_ty = type_name(&node.self_ty);
    match &node.trait_ {
        Some((_, path, _)) => {
            let trait_name = path
                .segments
                .last()
                .map(|s| s.ident.to_string())
                .unwrap_or_default();
            format!("<{self_ty} as {trait_name}>")
        }
        None => format!("impl {self_ty}"),
    }
}

fn type_name(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(p) => p
            .path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect::<Vec<_>>()
            .join("::"),
        _ => "?".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_fn() {
        let src = "pub fn hello() {}";
        let items = extract_items(src).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "hello");
        assert_eq!(items[0].kind, Kind::Fn);
    }

    #[test]
    fn extracts_struct() {
        let src = "pub struct Foo { x: i32 }";
        let items = extract_items(src).unwrap();
        assert_eq!(items[0].kind, Kind::Struct);
        assert_eq!(items[0].name, "Foo");
    }

    #[test]
    fn extracts_enum() {
        let src = "pub enum Bar { A, B }";
        let items = extract_items(src).unwrap();
        assert_eq!(items[0].kind, Kind::Enum);
        assert_eq!(items[0].name, "Bar");
    }

    #[test]
    fn extracts_trait() {
        let src = "pub trait MyTrait { fn method(&self); }";
        let items = extract_items(src).unwrap();
        assert_eq!(items[0].kind, Kind::Trait);
        assert_eq!(items[0].name, "MyTrait");
    }

    #[test]
    fn extracts_impl_inherent() {
        let src = "impl Foo { fn new() -> Self { Foo } }";
        let items = extract_items(src).unwrap();
        assert_eq!(items[0].kind, Kind::Impl);
        assert_eq!(items[0].name, "impl Foo");
    }

    #[test]
    fn extracts_impl_trait() {
        let src = "impl MyTrait for Foo {}";
        let items = extract_items(src).unwrap();
        assert_eq!(items[0].kind, Kind::Impl);
        assert_eq!(items[0].name, "<Foo as MyTrait>");
    }

    #[test]
    fn extracts_mod() {
        let src = "pub mod inner { pub fn x() {} }";
        let items = extract_items(src).unwrap();
        // Only the mod itself — we don't recurse into inline mods.
        assert!(
            items
                .iter()
                .any(|i| i.kind == Kind::Mod && i.name == "inner")
        );
    }

    #[test]
    fn extracts_const() {
        let src = "pub const MAX: usize = 42;";
        let items = extract_items(src).unwrap();
        assert_eq!(items[0].kind, Kind::Const);
        assert_eq!(items[0].name, "MAX");
    }

    #[test]
    fn returns_error_on_invalid_syntax() {
        let result = extract_items("fn foo( { }");
        assert!(result.is_err());
    }

    #[test]
    fn line_numbers_populated() {
        let src = "pub fn first() {}\npub fn second() {}";
        let items = extract_items(src).unwrap();
        assert_eq!(items[0].line_start, 1);
        assert_eq!(items[1].line_start, 2);
    }

    #[test]
    fn multi_line_fn_spans_full_body() {
        let src = "pub fn hello(\n    x: i32,\n) -> i32 {\n    x + 1\n}";
        let items = extract_items(src).unwrap();
        assert_eq!(items[0].line_start, 1);
        assert!(
            items[0].line_end > items[0].line_start,
            "multi-line fn must have line_end > line_start, got start={} end={}",
            items[0].line_start,
            items[0].line_end
        );
    }

    #[test]
    fn multi_line_struct_spans_full_body() {
        let src = "pub struct Foo {\n    x: i32,\n    y: i32,\n}";
        let items = extract_items(src).unwrap();
        assert!(
            items[0].line_end > items[0].line_start,
            "multi-line struct must have line_end > line_start"
        );
    }

    #[test]
    fn multi_line_impl_spans_full_body() {
        let src =
            "impl Foo {\n    pub fn new() -> Self { Foo }\n    pub fn val(&self) -> i32 { 0 }\n}";
        let items = extract_items(src).unwrap();
        let impl_item = items.iter().find(|i| i.kind == Kind::Impl).unwrap();
        assert!(
            impl_item.line_end > impl_item.line_start,
            "multi-line impl must have line_end > line_start"
        );
    }

    #[test]
    fn extracts_multiple_items() {
        let src = r#"
pub struct Config { pub host: String }
pub fn run(cfg: Config) {}
pub const VERSION: &str = "1.0";
"#;
        let items = extract_items(src).unwrap();
        assert_eq!(items.len(), 3);
    }
}
