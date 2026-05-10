//! CALLS relation extraction from source body.
//!
//! Parses source text to find direct function call expressions and emits
//! `(caller_fqn, callee_fqn, CALLS)` relations.

use std::collections::HashSet;

use rb_schemas::RelationKind;
use syn::visit::Visit;

use crate::extractor::Relation;

/// Parse `body` with `syn` to find direct function call expressions.
///
/// Emits (`fqn`, `callee_fqn`, CALLS) for each unique callee found in the body.
/// Best-effort: unqualified single-segment calls are resolved to the same module
/// as the caller (e.g. `bar()` in `src_mod::caller` → `src_mod::bar`).
/// Calls via `self`, `super`, `crate`, `std`, `core`, or `alloc` are skipped.
pub(crate) fn extract_call_relations(fqn: &str, body: &str, out: &mut Vec<Relation>) {
    let file: syn::File = match syn::parse_str(body) {
        Ok(f) => f,
        Err(_) => return,
    };
    let caller_module = fqn.rsplit_once("::").map_or("", |(m, _)| m).to_owned();
    let mut visitor = CallVisitor {
        caller_fqn: fqn.to_owned(),
        caller_module,
        relations: Vec::new(),
        seen: HashSet::new(),
    };
    visitor.visit_file(&file);
    out.extend(visitor.relations);
}

pub(crate) struct CallVisitor {
    pub(crate) caller_fqn: String,
    pub(crate) caller_module: String,
    pub(crate) relations: Vec<Relation>,
    pub(crate) seen: HashSet<String>,
}

impl<'ast> Visit<'ast> for CallVisitor {
    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path_expr) = node.func.as_ref() {
            if let Some(first) = path_expr.path.segments.first() {
                let f = first.ident.to_string();
                if matches!(
                    f.as_str(),
                    "self" | "super" | "crate" | "std" | "core" | "alloc"
                ) {
                    syn::visit::visit_expr_call(self, node);
                    return;
                }
            }
            let segs: Vec<String> = path_expr
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            if segs.is_empty() {
                syn::visit::visit_expr_call(self, node);
                return;
            }
            let callee_fqn = if segs.len() == 1 && !self.caller_module.is_empty() {
                format!("{}::{}", self.caller_module, segs[0])
            } else {
                segs.join("::")
            };
            if callee_fqn != self.caller_fqn && self.seen.insert(callee_fqn.clone()) {
                self.relations.push(Relation {
                    from_fqn: self.caller_fqn.clone(),
                    to_fqn: callee_fqn,
                    kind: RelationKind::Calls,
                });
            }
        }
        syn::visit::visit_expr_call(self, node);
    }
}
