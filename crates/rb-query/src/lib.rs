//! `rb-query` — tenant-scoped read queries for the rust-brain code graph.
//!
//! Provides SQL helpers that operate against per-tenant schemas via
//! fully-qualified table names (`TenantCtx::qualify`). Never mutates data.
//!
//! The `graph` module provides Neo4j read queries routed through
//! [`rb_storage_neo4j::TenantGraph`] for tenant isolation (ADR-007 §3.4).
//! The `vector` module provides semantic search via [`rb_storage_qdrant::TenantVectorStore`]
//! with mandatory per-tenant isolation (ADR-007 §13.2).
//! The `hybrid` module fuses dense + sparse legs via RRF (ADR-014 §3).

mod error;
mod graph;
pub mod hybrid;
mod pg;
mod semantic;
mod vector;

pub use error::QueryError;
pub use graph::impls::{ImplEntry, fetch_trait_impls};
pub use graph::traversal::{
    DEFAULT_DEPTH, DEFAULT_LIMIT, EdgeProvenance, MAX_DEPTH, MAX_LIMIT, TraversalEdge,
    TraversalNode, TraversalOptions, TraversalResult, fetch_callees, fetch_callers,
};
pub use graph::usages::{UsageEntry, fetch_type_usages};
pub use hybrid::{HybridHit, HybridSearchOptions, hybrid_search};
pub use pg::items;
pub use pg::modules::{ModuleNode, ModuleTreeCache, fetch_module_tree, new_module_tree_cache};
pub use semantic::{SemanticSearchError, search_by_vector};
pub use vector::search::{
    DEFAULT_SEARCH_LIMIT, MAX_SEARCH_LIMIT, SearchOptions, SemanticHit, semantic_search,
};
