/// Error type for tenant-scoped query operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum QueryError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}
