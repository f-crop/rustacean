/// Error type for tenant-scoped query operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum QueryError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("graph error: {0}")]
    Graph(#[from] rb_storage_neo4j::CypherError),
    #[error("vector store error: {0}")]
    Qdrant(#[from] rb_storage_qdrant::QdrantError),
}
