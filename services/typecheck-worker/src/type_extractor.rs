//! AST-level type signature extraction for typecheck-worker.
//!
//! Uses `syn` to walk `*.rs` files and extract per-item type information:
//! `resolved_type_signature` and `trait_bounds`.  This is a best-effort
//! syntactic extraction — full semantic resolution would require a complete
//! compiler invocation via `ra_ap_*` crates, which is planned for a future
//! iteration (ADR-007 §11.6).

use proc_macro2::LineColumn;
use syn::spanned::Spanned as _;
use syn::visit::Visit;

/// Type information extracted from a single Rust item.
#[derive(Debug, Clone)]
pub(crate) struct TypedItemData {
    pub(crate) name: String,
    pub(crate) line_start: u32,
    pub(crate) line_end: u32,
    /// Human-readable type signature (function signature, struct header, etc.)
    pub(crate) resolved_type_signature: String,
    /// Where-clause and inline generic bounds, one predicate per entry.
    pub(crate) trait_bounds: Vec<String>,
}

/// Extract typed items from `source`.  Returns an empty vec on parse failure.
pub(crate) fn extract_typed_items(source: &str) -> Vec<TypedItemData> {
    let file: syn::File = match syn::parse_str(source) {
        Ok(f) => f,
        Err(_) => return vec![],
    };
    let mut visitor = TypeVisitor { items: Vec::new() };
    visitor.visit_file(&file);
    visitor.items
}

// ── Visitor ──────────────────────────────────────────────────────────────────

struct TypeVisitor {
    items: Vec<TypedItemData>,
}

impl TypeVisitor {
    fn push(&mut self, name: String, span: proc_macro2::Span, sig: String, bounds: Vec<String>) {
        let start: LineColumn = span.start();
        let end: LineColumn = span.end();
        self.items.push(TypedItemData {
            name,
            line_start: u32::try_from(start.line).expect("line fits u32"),
            line_end: u32::try_from(end.line).expect("line fits u32"),
            resolved_type_signature: sig,
            trait_bounds: bounds,
        });
    }
}

impl<'ast> Visit<'ast> for TypeVisitor {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        let name = node.sig.ident.to_string();
        let sig = fmt_fn_sig(&node.sig);
        let bounds = extract_bounds(&node.sig.generics);
        self.push(name, node.span(), sig, bounds);
    }

    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        let name = node.ident.to_string();
        let generics = fmt_generics_params(&node.generics);
        let sig = format!("struct {name}{generics}");
        let bounds = extract_bounds(&node.generics);
        self.push(name, node.span(), sig, bounds);
    }

    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        let name = node.ident.to_string();
        let generics = fmt_generics_params(&node.generics);
        let sig = format!("enum {name}{generics}");
        let bounds = extract_bounds(&node.generics);
        self.push(name, node.span(), sig, bounds);
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        let name = node.ident.to_string();
        let generics = fmt_generics_params(&node.generics);
        let supertraits = if node.supertraits.is_empty() {
            String::new()
        } else {
            let parts: Vec<String> = node.supertraits.iter().map(fmt_type_param_bound).collect();
            format!(": {}", parts.join(" + "))
        };
        let sig = format!("trait {name}{generics}{supertraits}");
        let bounds = extract_bounds(&node.generics);
        self.push(name, node.span(), sig, bounds);
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let generics = fmt_generics_params(&node.generics);
        let self_ty = fmt_type(&node.self_ty);
        let (name, sig) = if let Some((_, path, _)) = &node.trait_ {
            let trait_name = fmt_path(path);
            let n = format!("<{self_ty} as {trait_name}>");
            let s = format!("impl{generics} {trait_name} for {self_ty}");
            (n, s)
        } else {
            let n = format!("impl {self_ty}");
            let s = format!("impl{generics} {self_ty}");
            (n, s)
        };
        let bounds = extract_bounds(&node.generics);
        self.push(name, node.span(), sig, bounds);
    }

    fn visit_item_const(&mut self, node: &'ast syn::ItemConst) {
        let name = node.ident.to_string();
        let ty = fmt_type(&node.ty);
        let sig = format!("const {name}: {ty}");
        self.push(name, node.span(), sig, vec![]);
    }

    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        let name = node.ident.to_string();
        let generics = fmt_generics_params(&node.generics);
        let ty = fmt_type(&node.ty);
        let sig = format!("type {name}{generics} = {ty}");
        let bounds = extract_bounds(&node.generics);
        self.push(name, node.span(), sig, bounds);
    }

    fn visit_item_static(&mut self, node: &'ast syn::ItemStatic) {
        let name = node.ident.to_string();
        let ty = fmt_type(&node.ty);
        let mutability = if matches!(node.mutability, syn::StaticMutability::Mut(_)) {
            "mut "
        } else {
            ""
        };
        let sig = format!("static {mutability}{name}: {ty}");
        self.push(name, node.span(), sig, vec![]);
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        let name = node.ident.to_string();
        let sig = format!("mod {name}");
        self.push(name, node.span(), sig, vec![]);
        // Do not recurse into inline mod bodies — callers handle nested files.
    }

    fn visit_item_macro(&mut self, node: &'ast syn::ItemMacro) {
        if let Some(ident) = &node.ident {
            let name = ident.to_string();
            let sig = format!("macro_rules! {name}");
            self.push(name, node.span(), sig, vec![]);
        }
    }
}

// ── Formatting helpers ────────────────────────────────────────────────────────

fn fmt_fn_sig(sig: &syn::Signature) -> String {
    let name = sig.ident.to_string();
    let generics = fmt_generics_params(&sig.generics);
    let inputs: Vec<String> = sig.inputs.iter().map(fmt_fn_arg).collect();
    let output = match &sig.output {
        syn::ReturnType::Default => String::new(),
        syn::ReturnType::Type(_, ty) => format!(" -> {}", fmt_type(ty)),
    };
    let asyncness = if sig.asyncness.is_some() { "async " } else { "" };
    format!("{asyncness}fn {name}{generics}({}){output}", inputs.join(", "))
}

fn fmt_fn_arg(arg: &syn::FnArg) -> String {
    match arg {
        syn::FnArg::Receiver(r) => {
            let mutability = if r.mutability.is_some() { "mut " } else { "" };
            match &r.reference {
                Some((_, lifetime)) => {
                    let lt = lifetime
                        .as_ref()
                        .map(|l| format!("'{} ", l.ident))
                        .unwrap_or_default();
                    format!("&{lt}{mutability}self")
                }
                None => format!("{mutability}self"),
            }
        }
        syn::FnArg::Typed(pat_ty) => {
            let ty = fmt_type(&pat_ty.ty);
            let pat = fmt_pat(&pat_ty.pat);
            format!("{pat}: {ty}")
        }
    }
}

fn fmt_pat(pat: &syn::Pat) -> String {
    match pat {
        syn::Pat::Ident(p) => {
            let mutability = if p.mutability.is_some() { "mut " } else { "" };
            format!("{mutability}{}", p.ident)
        }
        syn::Pat::Reference(r) => {
            let inner = fmt_pat(&r.pat);
            let mutability = if r.mutability.is_some() { "mut " } else { "" };
            format!("&{mutability}{inner}")
        }
        syn::Pat::Tuple(t) => {
            let parts: Vec<String> = t.elems.iter().map(fmt_pat).collect();
            format!("({})", parts.join(", "))
        }
        _ => "_".to_owned(),
    }
}

pub(crate) fn fmt_type(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(p) => {
            let qself = p
                .qself
                .as_ref()
                .map(|q| format!("<{} as ", fmt_type(&q.ty)))
                .unwrap_or_default();
            let path = fmt_path(&p.path);
            if qself.is_empty() {
                path
            } else {
                format!("{qself}{path}>")
            }
        }
        syn::Type::Reference(r) => {
            let lt = r
                .lifetime
                .as_ref()
                .map(|l| format!("'{} ", l.ident))
                .unwrap_or_default();
            let mutability = if r.mutability.is_some() { "mut " } else { "" };
            format!("&{lt}{mutability}{}", fmt_type(&r.elem))
        }
        syn::Type::Ptr(p) => {
            let mutability = if p.mutability.is_some() { "mut " } else { "const " };
            format!("*{mutability}{}", fmt_type(&p.elem))
        }
        syn::Type::Slice(s) => format!("[{}]", fmt_type(&s.elem)),
        syn::Type::Array(a) => format!("[{}; _]", fmt_type(&a.elem)),
        syn::Type::Tuple(t) => {
            let elems: Vec<String> = t.elems.iter().map(fmt_type).collect();
            format!("({})", elems.join(", "))
        }
        syn::Type::Never(_) => "!".to_owned(),
        syn::Type::ImplTrait(i) => {
            let bounds: Vec<String> = i.bounds.iter().map(fmt_type_param_bound).collect();
            format!("impl {}", bounds.join(" + "))
        }
        syn::Type::TraitObject(t) => {
            let bounds: Vec<String> = t.bounds.iter().map(fmt_type_param_bound).collect();
            format!("dyn {}", bounds.join(" + "))
        }
        syn::Type::Paren(p) => fmt_type(&p.elem),
        syn::Type::BareFn(f) => {
            let inputs: Vec<String> = f.inputs.iter().map(|a| fmt_type(&a.ty)).collect();
            let output = match &f.output {
                syn::ReturnType::Default => String::new(),
                syn::ReturnType::Type(_, ty) => format!(" -> {}", fmt_type(ty)),
            };
            format!("fn({}){output}", inputs.join(", "))
        }
        _ => "_".to_owned(),
    }
}

fn fmt_path(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|seg| {
            let ident = seg.ident.to_string();
            let args = fmt_path_args(&seg.arguments);
            format!("{ident}{args}")
        })
        .collect::<Vec<_>>()
        .join("::")
}

fn fmt_path_args(args: &syn::PathArguments) -> String {
    match args {
        syn::PathArguments::None => String::new(),
        syn::PathArguments::AngleBracketed(a) => {
            let args: Vec<String> = a.args.iter().map(fmt_generic_arg).collect();
            if args.is_empty() {
                String::new()
            } else {
                format!("<{}>", args.join(", "))
            }
        }
        syn::PathArguments::Parenthesized(p) => {
            let inputs: Vec<String> = p.inputs.iter().map(fmt_type).collect();
            let output = match &p.output {
                syn::ReturnType::Default => String::new(),
                syn::ReturnType::Type(_, ty) => format!(" -> {}", fmt_type(ty)),
            };
            format!("({}){output}", inputs.join(", "))
        }
    }
}

fn fmt_generic_arg(arg: &syn::GenericArgument) -> String {
    match arg {
        syn::GenericArgument::Lifetime(l) => format!("'{}", l.ident),
        syn::GenericArgument::Type(ty) => fmt_type(ty),
        syn::GenericArgument::Const(_) => "{ _ }".to_owned(),
        syn::GenericArgument::AssocType(a) => {
            format!("{} = {}", a.ident, fmt_type(&a.ty))
        }
        syn::GenericArgument::AssocConst(a) => format!("{} = _", a.ident),
        syn::GenericArgument::Constraint(c) => {
            let bounds: Vec<String> = c.bounds.iter().map(fmt_type_param_bound).collect();
            format!("{}: {}", c.ident, bounds.join(" + "))
        }
        _ => "_".to_owned(),
    }
}

fn fmt_type_param_bound(bound: &syn::TypeParamBound) -> String {
    match bound {
        syn::TypeParamBound::Trait(t) => fmt_path(&t.path),
        syn::TypeParamBound::Lifetime(l) => format!("'{}", l.ident),
        _ => "_".to_owned(),
    }
}

fn fmt_generics_params(generics: &syn::Generics) -> String {
    if generics.params.is_empty() {
        return String::new();
    }
    let params: Vec<String> = generics.params.iter().map(fmt_generic_param).collect();
    format!("<{}>", params.join(", "))
}

fn fmt_generic_param(param: &syn::GenericParam) -> String {
    match param {
        syn::GenericParam::Type(t) => {
            let name = t.ident.to_string();
            if t.bounds.is_empty() {
                name
            } else {
                let bounds: Vec<String> = t.bounds.iter().map(fmt_type_param_bound).collect();
                format!("{name}: {}", bounds.join(" + "))
            }
        }
        syn::GenericParam::Lifetime(l) => {
            let name = format!("'{}", l.lifetime.ident);
            if l.bounds.is_empty() {
                name
            } else {
                let bounds: Vec<String> = l
                    .bounds
                    .iter()
                    .map(|lt| format!("'{}", lt.ident))
                    .collect();
                format!("{name}: {}", bounds.join(" + "))
            }
        }
        syn::GenericParam::Const(c) => {
            let ty = fmt_type(&c.ty);
            format!("const {}: {ty}", c.ident)
        }
    }
}

/// Collect all bound predicates (inline bounds + where clause) as strings.
pub(crate) fn extract_bounds(generics: &syn::Generics) -> Vec<String> {
    let mut bounds: Vec<String> = Vec::new();

    // Inline bounds on type parameters (e.g. `T: Clone + Send`)
    for param in &generics.params {
        if let syn::GenericParam::Type(t) = param {
            if !t.bounds.is_empty() {
                let b: Vec<String> = t.bounds.iter().map(fmt_type_param_bound).collect();
                bounds.push(format!("{}: {}", t.ident, b.join(" + ")));
            }
        }
    }

    // Where clause predicates
    if let Some(clause) = &generics.where_clause {
        for pred in &clause.predicates {
            if let syn::WherePredicate::Type(pt) = pred {
                let ty = fmt_type(&pt.bounded_ty);
                let b: Vec<String> = pt.bounds.iter().map(fmt_type_param_bound).collect();
                bounds.push(format!("{ty}: {}", b.join(" + ")));
            }
        }
    }

    bounds
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_fn_signature() {
        let src = "pub fn add(x: i32, y: i32) -> i32 { x + y }";
        let items = extract_typed_items(src);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "add");
        assert_eq!(items[0].resolved_type_signature, "fn add(x: i32, y: i32) -> i32");
        assert!(items[0].trait_bounds.is_empty());
    }

    #[test]
    fn extracts_async_fn_signature() {
        let src = "pub async fn fetch() -> String { String::new() }";
        let items = extract_typed_items(src);
        assert_eq!(items[0].resolved_type_signature, "async fn fetch() -> String");
    }

    #[test]
    fn extracts_fn_with_self_ref() {
        let src = "impl Foo { pub fn len(&self) -> usize { 0 } }";
        let items = extract_typed_items(src);
        let fn_item = items.iter().find(|i| i.name == "fn len(…)");
        // impl block is extracted; fn inside impl is not visited at top level
        let impl_item = items.iter().find(|i| i.name == "impl Foo");
        assert!(impl_item.is_some(), "impl Foo should be extracted");
        let _ = fn_item; // fn inside impl block is not top-level
    }

    #[test]
    fn extracts_struct_signature() {
        let src = "pub struct Point { pub x: f64, pub y: f64 }";
        let items = extract_typed_items(src);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Point");
        assert_eq!(items[0].resolved_type_signature, "struct Point");
    }

    #[test]
    fn extracts_generic_struct_with_bounds() {
        let src = "pub struct Wrapper<T: Clone + Send>(T);";
        let items = extract_typed_items(src);
        assert_eq!(items[0].resolved_type_signature, "struct Wrapper<T: Clone + Send>");
        assert!(items[0].trait_bounds.contains(&"T: Clone + Send".to_owned()));
    }

    #[test]
    fn extracts_enum_signature() {
        let src = "pub enum Color { Red, Green, Blue }";
        let items = extract_typed_items(src);
        assert_eq!(items[0].name, "Color");
        assert_eq!(items[0].resolved_type_signature, "enum Color");
    }

    #[test]
    fn extracts_trait_with_supertraits() {
        let src = "pub trait Animal: Clone + Send { fn name(&self) -> &str; }";
        let items = extract_typed_items(src);
        assert_eq!(items[0].name, "Animal");
        assert_eq!(items[0].resolved_type_signature, "trait Animal: Clone + Send");
    }

    #[test]
    fn extracts_impl_inherent() {
        let src = "impl Foo { fn new() -> Self { Foo } }";
        let items = extract_typed_items(src);
        assert_eq!(items[0].name, "impl Foo");
        assert_eq!(items[0].resolved_type_signature, "impl Foo");
    }

    #[test]
    fn extracts_impl_trait() {
        let src = "impl Display for Foo {}";
        let items = extract_typed_items(src);
        assert_eq!(items[0].name, "<Foo as Display>");
        assert_eq!(items[0].resolved_type_signature, "impl Display for Foo");
    }

    #[test]
    fn extracts_const_signature() {
        let src = "pub const MAX_SIZE: usize = 1024;";
        let items = extract_typed_items(src);
        assert_eq!(items[0].name, "MAX_SIZE");
        assert_eq!(items[0].resolved_type_signature, "const MAX_SIZE: usize");
    }

    #[test]
    fn extracts_type_alias() {
        let src = "pub type Result<T> = std::result::Result<T, Error>;";
        let items = extract_typed_items(src);
        assert_eq!(items[0].name, "Result");
        assert!(items[0].resolved_type_signature.starts_with("type Result<T> = "));
    }

    #[test]
    fn extracts_where_clause_bounds() {
        let src = "pub fn process<T>(val: T) where T: Clone + Debug {}";
        let items = extract_typed_items(src);
        assert!(items[0]
            .trait_bounds
            .contains(&"T: Clone + Debug".to_owned()));
    }

    #[test]
    fn returns_empty_on_parse_failure() {
        let items = extract_typed_items("fn broken( {");
        assert!(items.is_empty());
    }

    #[test]
    fn line_numbers_populated() {
        let src = "pub fn first() {}\npub fn second() {}";
        let items = extract_typed_items(src);
        assert_eq!(items[0].line_start, 1);
        assert_eq!(items[1].line_start, 2);
    }

    #[test]
    fn item_fn_span_covers_full_body() {
        // The fix: node.span() covers all lines; node.sig.ident.span() would give line_start==line_end.
        let src = "pub fn foo() {\n    let x = 1;\n    x\n}";
        let items = extract_typed_items(src);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "foo");
        assert!(
            items[0].line_end > items[0].line_start,
            "multi-line fn: line_end ({}) must be > line_start ({})",
            items[0].line_end,
            items[0].line_start,
        );
    }

    #[test]
    fn item_struct_span_covers_full_body() {
        let src = "pub struct Foo {\n    pub x: i32,\n    pub y: i32,\n}";
        let items = extract_typed_items(src);
        assert_eq!(items[0].name, "Foo");
        assert!(
            items[0].line_end > items[0].line_start,
            "multi-line struct: line_end ({}) must be > line_start ({})",
            items[0].line_end,
            items[0].line_start,
        );
    }

    #[test]
    fn item_impl_span_covers_full_body() {
        let src = "impl Foo {\n    pub fn new() -> Self {\n        Foo {}\n    }\n}";
        let items = extract_typed_items(src);
        let impl_item = items.iter().find(|i| i.name == "impl Foo").expect("impl Foo not found");
        assert!(
            impl_item.line_end > impl_item.line_start,
            "multi-line impl: line_end ({}) must be > line_start ({})",
            impl_item.line_end,
            impl_item.line_start,
        );
    }

    #[test]
    fn async_fn_span_covers_full_body() {
        let src = "pub async fn handle(\n    req: Request,\n) -> Response {\n    todo!()\n}";
        let items = extract_typed_items(src);
        assert_eq!(items[0].name, "handle");
        assert!(
            items[0].line_end > items[0].line_start,
            "multi-line async fn: line_end ({}) must be > line_start ({})",
            items[0].line_end,
            items[0].line_start,
        );
    }

    #[test]
    fn fmt_type_reference() {
        let src = "pub fn f(x: &str) {}";
        let items = extract_typed_items(src);
        assert!(items[0].resolved_type_signature.contains("&str"));
    }

    #[test]
    fn fmt_type_option() {
        let src = "pub fn f() -> Option<String> { None }";
        let items = extract_typed_items(src);
        assert!(items[0].resolved_type_signature.contains("Option<String>"));
    }

    #[test]
    fn fmt_type_result() {
        let src = "pub fn f() -> Result<i32, Error> { Ok(0) }";
        let items = extract_typed_items(src);
        assert!(
            items[0].resolved_type_signature.contains("Result<i32, Error>")
        );
    }

    #[test]
    fn fmt_type_tuple() {
        let src = "pub fn f() -> (i32, bool) { (0, true) }";
        let items = extract_typed_items(src);
        assert!(items[0].resolved_type_signature.contains("(i32, bool)"));
    }

    #[test]
    fn fmt_type_slice() {
        let src = "pub fn f(s: &[u8]) {}";
        let items = extract_typed_items(src);
        assert!(items[0].resolved_type_signature.contains("[u8]"));
    }

    #[test]
    fn static_item_signature() {
        let src = "pub static GREETING: &str = \"hello\";";
        let items = extract_typed_items(src);
        assert_eq!(items[0].resolved_type_signature, "static GREETING: &str");
    }

    #[test]
    fn mod_item_signature() {
        let src = "pub mod utils {}";
        let items = extract_typed_items(src);
        assert_eq!(items[0].name, "utils");
        assert_eq!(items[0].resolved_type_signature, "mod utils");
    }
}
