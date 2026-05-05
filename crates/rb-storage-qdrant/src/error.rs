use thiserror::Error;

#[derive(Debug, Error)]
pub enum QdrantError {
    #[error("tenant_id filter is required — refusing to execute an unscoped query")]
    MissingTenantFilter,

    #[error("Qdrant HTTP error {status}: {body}")]
    Http { status: u16, body: String },

    #[error("Qdrant request failed: {0}")]
    Request(#[from] reqwest::Error),

    #[error("Qdrant response parse error: {0}")]
    Parse(String),
}
