//! `rb-query` — tenant-scoped read queries for the rust-brain code graph.
//!
//! Provides SQL helpers that operate against per-tenant schemas via
//! fully-qualified table names (`TenantCtx::qualify`). Never mutates data.

mod error;
mod pg;

pub use error::QueryError;
<<<<<<< HEAD
pub use pg::items;
=======
>>>>>>> 725e519 (fix(rb-query, control-api): CI fixes for RUSAA-77 PR #199)
pub use pg::modules::{ModuleNode, ModuleTreeCache, fetch_module_tree, new_module_tree_cache};
