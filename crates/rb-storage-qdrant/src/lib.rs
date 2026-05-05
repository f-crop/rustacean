//! `rb-storage-qdrant` — thin REST wrapper around the Qdrant vector database.
//!
//! Provides a [`search`] function for performing tenant-scoped ANN queries
//! against the shared `rb_embeddings` collection (ADR-008 §2).
//!
//! Every query MUST include a `tenant_id` `must` filter to prevent
//! cross-tenant data leakage.

pub mod error;
pub mod search;

pub use error::QdrantError;
pub use search::{SearchHit, SearchOptions, SearchResults, search};

/// Name of the single shared Qdrant collection for all tenants.
pub const COLLECTION: &str = "rb_embeddings";

/// Default minimum cosine-similarity score for a Qdrant hit to be returned.
pub const DEFAULT_SCORE_FLOOR: f32 = 0.20;
