//! `rb-storage-qdrant` — tenant-scoped Qdrant vector search wrapper.
//!
//! Provides [`TenantVectorStore`] as the sole entry point for searching
//! the `rb_embeddings` Qdrant collection.  Every search injects a mandatory
//! `tenant_id` `must` filter (ADR-007 §13.2) so cross-tenant data is never
//! reachable even if a call site contains a bug.
//!
//! No code outside this crate may issue raw Qdrant REST requests — CI lint
//! enforces this boundary (analogous to `rb-storage-neo4j`).

mod error;
pub mod search;
mod store;

pub use error::QdrantError;
pub use store::{SearchHit, TenantVectorStore};
pub use search::{SearchOptions, SearchResults, search};

/// Name of the single shared Qdrant collection for all tenants.
pub const COLLECTION: &str = "rb_embeddings";

/// Default minimum cosine-similarity score for a Qdrant hit to be returned.
pub const DEFAULT_SCORE_FLOOR: f32 = 0.20;
