//! `rb-query` — tenant-scoped read queries for the rust-brain code graph.
//!
//! Provides SQL helpers that operate against per-tenant schemas via
//! fully-qualified table names (`TenantCtx::qualify`). Never mutates data.

pub mod error;
pub mod pg;

pub use error::QueryError;
