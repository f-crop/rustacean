/// Error type for Qdrant REST operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum QdrantError {
    #[error("Qdrant HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Qdrant returned HTTP {status}: {body}")]
    Api { status: u16, body: String },
}
