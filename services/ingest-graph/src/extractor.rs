//! Relation extraction from [`TypecheckedItemEvent`] payloads.
//!
//! Produces [`Relation`] values (`from_fqn`, `to_fqn`, kind) from four sources:
//!   1. `resolved_type_signature` — impl blocks and trait supertrait declarations.
//!   2. `trait_bounds` — bounded-by predicates emitted by typecheck-worker.
//!   3. Source body (if present) — derive attributes parsed with `syn`.
//!   4. Source body (if present) — function call expressions parsed with `syn` (CALLS).

use rb_schemas::RelationKind;
use syn::visit::Visit;

pub(crate) use crate::extractor_calls::extract_call_relations;

/// A single directed graph relation extracted from one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Relation {
    pub(crate) from_fqn: String,
    pub(crate) to_fqn: String,
    pub(crate) kind: RelationKind,
}

/// Entry point: extract all relations from one typechecked item.
///
/// `fqn`   — fully-qualified name of the item (e.g. `src_lib::MyStruct`).
/// `sig`   — `resolved_type_signature` string from [`TypecheckedItemEvent`].
/// `bounds` — `trait_bounds` repeated field from [`TypecheckedItemEvent`].
/// `body`  — source text of the item; may be empty if unavailable.
pub(crate) fn extract_relations(
    fqn: &str,
    sig: &str,
    bounds: &[String],
    body: &str,
) -> Vec<Relation> {
    let mut out = Vec::new();
    extract_impl_relation(fqn, sig, &mut out);
    extract_supertrait_relations(fqn, sig, &mut out);
    extract_bound_relations(fqn, bounds, &mut out);
    if !body.is_empty() {
        extract_derive_relations(fqn, body, &mut out);
        extract_call_relations(fqn, body, &mut out);
    }
    out
}

// ── IMPLS ─────────────────────────────────────────────────────────────────────

/// `impl Display for Foo` → (`Foo`, `Display`, IMPLS).
///
/// Handles:
///   `impl Trait for Type`
///   `impl<T> Trait<T> for Type<T>`
///
/// Inherent impls (`impl Foo`) are ignored — they produce no IMPLS relation.
fn extract_impl_relation(fqn: &str, sig: &str, out: &mut Vec<Relation>) {
    if !sig.starts_with("impl") {
        return;
    }
    let Some(for_pos) = sig.find(" for ") else {
        return;
    };
    let trait_part = sig[..for_pos].trim_start_matches("impl").trim();
    let trait_name = strip_leading_generics(trait_part);
    let type_part = sig[for_pos + 5..].trim();
    let type_name = first_ident_segment(type_part);

    if trait_name.is_empty() || type_name.is_empty() {
        return;
    }

    let from = if fqn.contains(" as ") || type_name == strip_generics(type_part) {
        type_name.to_owned()
    } else {
        fqn.to_owned()
    };

    out.push(Relation {
        from_fqn: from,
        to_fqn: trait_name.to_owned(),
        kind: RelationKind::Impls,
    });
}

// ── EXTENDS_TRAIT ─────────────────────────────────────────────────────────────

/// `trait Animal: Clone + Send` → (`Animal`, `Clone`, `EXTENDS_TRAIT`), …
fn extract_supertrait_relations(fqn: &str, sig: &str, out: &mut Vec<Relation>) {
    if !sig.starts_with("trait ") {
        return;
    }
    let Some(colon_pos) = sig.find(": ") else {
        return;
    };
    let supertrait_str = sig[colon_pos + 2..].trim();
    for part in supertrait_str.split('+') {
        let st = first_ident_segment(part.trim());
        if !st.is_empty() {
            out.push(Relation {
                from_fqn: fqn.to_owned(),
                to_fqn: st.to_owned(),
                kind: RelationKind::ExtendsTrait,
            });
        }
    }
}

// ── BOUNDED_BY ────────────────────────────────────────────────────────────────

/// `trait_bounds: ["T: Clone + Send"]` → (`fqn::T`, `Clone`, `BOUNDED_BY`), …
fn extract_bound_relations(fqn: &str, bounds: &[String], out: &mut Vec<Relation>) {
    for bound_str in bounds {
        let Some(colon_pos) = bound_str.find(": ") else {
            continue;
        };
        let type_param = bound_str[..colon_pos].trim();
        let bounds_part = bound_str[colon_pos + 2..].trim();
        let from = format!("{fqn}::{type_param}");
        for b in bounds_part.split('+') {
            let bound_name = first_ident_segment(b.trim());
            if !bound_name.is_empty() {
                out.push(Relation {
                    from_fqn: from.clone(),
                    to_fqn: bound_name.to_owned(),
                    kind: RelationKind::BoundedBy,
                });
            }
        }
    }
}

// ── DERIVES ───────────────────────────────────────────────────────────────────

/// Parse `body` with `syn` to find `#[derive(...)]` attributes.
///
/// Emits (`fqn`, `DeriveName`, DERIVES) for each derive macro.
fn extract_derive_relations(fqn: &str, body: &str, out: &mut Vec<Relation>) {
    let file: syn::File = match syn::parse_str(body) {
        Ok(f) => f,
        Err(_) => return,
    };
    let mut visitor = DeriveVisitor {
        fqn: fqn.to_owned(),
        relations: Vec::new(),
    };
    visitor.visit_file(&file);
    out.extend(visitor.relations);
}

struct DeriveVisitor {
    fqn: String,
    relations: Vec<Relation>,
}

impl DeriveVisitor {
    fn push_derives(&mut self, attrs: &[syn::Attribute]) {
        for attr in attrs {
            if !attr.path().is_ident("derive") {
                continue;
            }
            if let syn::Meta::List(list) = &attr.meta {
                if let Ok(nested) = list.parse_args_with(
                    syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
                ) {
                    for path in &nested {
                        let name = fmt_path_simple(path);
                        if !name.is_empty() {
                            self.relations.push(Relation {
                                from_fqn: self.fqn.clone(),
                                to_fqn: name,
                                kind: RelationKind::Derives,
                            });
                        }
                    }
                }
            }
        }
    }
}

impl<'ast> Visit<'ast> for DeriveVisitor {
    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        self.push_derives(&node.attrs);
    }
    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        self.push_derives(&node.attrs);
    }
    fn visit_item_union(&mut self, node: &'ast syn::ItemUnion) {
        self.push_derives(&node.attrs);
    }
}

// ── String helpers ─────────────────────────────────────────────────────────────

/// Return the first path segment (identifier) from a type string.
///
/// `Vec<T>` → `Vec`,  `std::fmt::Display` → `std::fmt::Display` (keeps path),
/// `<T as Clone>` → `T`.
fn first_ident_segment(s: &str) -> &str {
    let s = s.trim();
    let s = if s.starts_with('<') {
        s.find('>').map_or(s, |i| s[i + 1..].trim())
    } else {
        s
    };
    let end = s.find(['<', '(', ' ', '{', '>']).unwrap_or(s.len());
    &s[..end]
}

/// Strip leading generic parameter list from a signature fragment.
///
/// `<T: Clone> Trait<T>` → `Trait<T>`.
fn strip_leading_generics(s: &str) -> &str {
    let s = s.trim();
    if !s.starts_with('<') {
        return s;
    }
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return s[i + 1..].trim();
                }
            }
            _ => {}
        }
    }
    s
}

/// Strip generic arguments: `Vec<T>` → `Vec`.
fn strip_generics(s: &str) -> &str {
    s.find('<').map_or(s, |i| s[..i].trim())
}

/// Format a syn Path as a simple dot-joined string (last segment for simple paths).
fn fmt_path_simple(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::extract_call_relations;
    use super::*;

    #[test]
    fn impl_for_emits_impls_relation() {
        let rels = extract_relations("src_lib::Foo", "impl Display for Foo", &[], "");
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].kind, RelationKind::Impls);
        assert_eq!(rels[0].to_fqn, "Display");
    }

    #[test]
    fn inherent_impl_produces_no_relation() {
        let rels = extract_relations("src_lib::Foo", "impl Foo", &[], "");
        assert!(rels.is_empty(), "inherent impl must not produce a relation");
    }

    #[test]
    fn generic_impl_for_emits_impls_relation() {
        let rels = extract_relations(
            "<Vec<T> as Clone>",
            "impl<T: Clone> Clone for Vec<T>",
            &[],
            "",
        );
        assert!(
            rels.iter()
                .any(|r| r.kind == RelationKind::Impls && r.to_fqn == "Clone")
        );
    }

    #[test]
    fn trait_supertrait_emits_extends_trait() {
        let rels = extract_relations("src_lib::Animal", "trait Animal: Clone + Send", &[], "");
        assert_eq!(rels.len(), 2);
        let kinds: Vec<_> = rels.iter().map(|r| r.kind).collect();
        assert!(kinds.iter().all(|&k| k == RelationKind::ExtendsTrait));
        let to_fqns: Vec<_> = rels.iter().map(|r| r.to_fqn.as_str()).collect();
        assert!(to_fqns.contains(&"Clone"));
        assert!(to_fqns.contains(&"Send"));
    }

    #[test]
    fn trait_without_supertrait_emits_nothing() {
        let rels = extract_relations("src_lib::Marker", "trait Marker", &[], "");
        assert!(rels.is_empty());
    }

    #[test]
    fn bound_single_emits_bounded_by() {
        let rels = extract_relations(
            "src_lib::process",
            "fn process<T>(val: T)",
            &["T: Clone".to_owned()],
            "",
        );
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].kind, RelationKind::BoundedBy);
        assert_eq!(rels[0].from_fqn, "src_lib::process::T");
        assert_eq!(rels[0].to_fqn, "Clone");
    }

    #[test]
    fn bound_multiple_bounds_emits_multiple() {
        let rels = extract_relations(
            "src_lib::run",
            "fn run<T>()",
            &["T: Clone + Send + Sync".to_owned()],
            "",
        );
        let bounded: Vec<_> = rels
            .iter()
            .filter(|r| r.kind == RelationKind::BoundedBy)
            .collect();
        assert_eq!(bounded.len(), 3);
        let to_fqns: Vec<&str> = bounded.iter().map(|r| r.to_fqn.as_str()).collect();
        assert!(to_fqns.contains(&"Clone"));
        assert!(to_fqns.contains(&"Send"));
        assert!(to_fqns.contains(&"Sync"));
    }

    #[test]
    fn bound_multiple_type_params() {
        let rels = extract_relations(
            "src_lib::pair",
            "fn pair<A, B>()",
            &["A: Clone".to_owned(), "B: Debug".to_owned()],
            "",
        );
        let bounded: Vec<_> = rels
            .iter()
            .filter(|r| r.kind == RelationKind::BoundedBy)
            .collect();
        assert_eq!(bounded.len(), 2);
    }

    #[test]
    fn derive_struct_emits_derives() {
        let body = "#[derive(Clone, Debug)] pub struct Foo { pub x: i32 }";
        let rels = extract_relations("src_lib::Foo", "struct Foo", &[], body);
        let derives: Vec<_> = rels
            .iter()
            .filter(|r| r.kind == RelationKind::Derives)
            .collect();
        assert_eq!(derives.len(), 2);
        let to_fqns: Vec<&str> = derives.iter().map(|r| r.to_fqn.as_str()).collect();
        assert!(to_fqns.contains(&"Clone"));
        assert!(to_fqns.contains(&"Debug"));
    }

    #[test]
    fn derive_enum_emits_derives() {
        let body = "#[derive(PartialEq, Eq)] pub enum Color { Red, Green }";
        let rels = extract_relations("src_lib::Color", "enum Color", &[], body);
        let derives: Vec<_> = rels
            .iter()
            .filter(|r| r.kind == RelationKind::Derives)
            .collect();
        assert_eq!(derives.len(), 2);
    }

    #[test]
    fn no_derive_produces_no_derives_relations() {
        let body = "pub struct Bare { pub x: i32 }";
        let rels = extract_relations("src_lib::Bare", "struct Bare", &[], body);
        assert!(rels.iter().all(|r| r.kind != RelationKind::Derives));
    }

    #[test]
    fn derive_invalid_body_produces_no_panic() {
        let rels = extract_relations("src_lib::X", "struct X", &[], "fn broken( {");
        let _ = rels;
    }

    #[test]
    fn combined_all_relation_kinds() {
        let body = "#[derive(Clone)] pub struct Wrapper<T>(T);";
        let rels = extract_relations(
            "src_lib::Wrapper",
            "struct Wrapper<T: Clone>",
            &["T: Clone".to_owned()],
            body,
        );
        assert!(rels.iter().any(|r| r.kind == RelationKind::BoundedBy));
        assert!(rels.iter().any(|r| r.kind == RelationKind::Derives));
    }

    #[test]
    fn calls_simple_unqualified_resolves_to_same_module() {
        let body = "fn caller() { bar(); }";
        let rels = extract_relations("src_mod::caller", "", &[], body);
        let calls: Vec<_> = rels
            .iter()
            .filter(|r| r.kind == RelationKind::Calls)
            .collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].from_fqn, "src_mod::caller");
        assert_eq!(calls[0].to_fqn, "src_mod::bar");
    }

    #[test]
    fn calls_qualified_path_preserved() {
        let body = "fn caller() { other::helper(); }";
        let rels = extract_relations("src_mod::caller", "", &[], body);
        let calls: Vec<_> = rels
            .iter()
            .filter(|r| r.kind == RelationKind::Calls)
            .collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].to_fqn, "other::helper");
    }

    #[test]
    fn calls_deduplicates_repeated_callee() {
        let body = "fn caller() { bar(); bar(); bar(); }";
        let rels = extract_relations("src_mod::caller", "", &[], body);
        let calls: Vec<_> = rels
            .iter()
            .filter(|r| r.kind == RelationKind::Calls)
            .collect();
        assert_eq!(calls.len(), 1, "duplicate callee should be deduplicated");
    }

    #[test]
    fn calls_skips_self_receiver() {
        let body = "fn caller() { self::bar(); }";
        let rels = extract_relations("src_mod::caller", "", &[], body);
        let calls: Vec<_> = rels
            .iter()
            .filter(|r| r.kind == RelationKind::Calls)
            .collect();
        assert!(calls.is_empty(), "self-prefixed call should be skipped");
    }

    #[test]
    fn calls_skips_std_crate_prefix() {
        let body = "fn caller() { std::mem::drop(x); crate::util::help(); }";
        let rels = extract_relations("src_mod::caller", "", &[], body);
        let calls: Vec<_> = rels
            .iter()
            .filter(|r| r.kind == RelationKind::Calls)
            .collect();
        assert!(
            calls.is_empty(),
            "std/crate-prefixed calls should be skipped"
        );
    }

    #[test]
    fn calls_no_self_call() {
        let body = "fn caller() { caller(); }";
        let rels = extract_relations("src_mod::caller", "", &[], body);
        let calls: Vec<_> = rels
            .iter()
            .filter(|r| r.kind == RelationKind::Calls)
            .collect();
        assert!(
            calls.is_empty(),
            "a function calling itself should not emit a CALLS edge"
        );
    }

    #[test]
    fn calls_multiple_distinct_callees() {
        let body = "fn caller() { foo(); bar(); baz(); }";
        let rels = extract_relations("src_mod::caller", "", &[], body);
        let calls: Vec<_> = rels
            .iter()
            .filter(|r| r.kind == RelationKind::Calls)
            .collect();
        assert_eq!(calls.len(), 3);
    }

    #[test]
    fn calls_invalid_body_does_not_panic() {
        let rels = extract_relations("src_mod::caller", "", &[], "fn broken( {");
        let _ = rels;
    }

    #[test]
    fn first_ident_segment_simple() {
        assert_eq!(first_ident_segment("Vec<T>"), "Vec");
        assert_eq!(first_ident_segment("Display"), "Display");
        assert_eq!(
            first_ident_segment("std::fmt::Display"),
            "std::fmt::Display"
        );
    }

    #[test]
    fn strip_leading_generics_works() {
        assert_eq!(strip_leading_generics("<T: Clone> Trait<T>"), "Trait<T>");
        assert_eq!(strip_leading_generics("Trait"), "Trait");
        assert_eq!(strip_leading_generics("<T> Foo<T>"), "Foo<T>");
    }

    #[test]
    fn strip_generics_removes_params() {
        assert_eq!(strip_generics("Vec<T>"), "Vec");
        assert_eq!(strip_generics("Foo"), "Foo");
    }
}
