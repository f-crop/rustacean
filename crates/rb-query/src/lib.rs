//! `rb-query` — tenant-scoped read queries for the rust-brain code graph.
//!
//! Provides SQL helpers that operate against per-tenant schemas via
//! fully-qualified table names (`TenantCtx::qualify`). Never mutates data.

mod error;
mod pg;

pub use error::QueryError;
pub use pg::items;
pub use pg::modules::{ModuleNode, ModuleTreeCache, fetch_module_tree, new_module_tree_cache};
