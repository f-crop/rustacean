use thiserror::Error;

#[derive(Debug, Error)]
pub enum RerankerError {
    #[error("model inference failed: {0}")]
    Model(String),

    #[error("blocking task join failed: {0}")]
    Blocking(String),

    #[error("LLM request failed: {0}")]
    Llm(String),

    #[error("LLM score parse failed: {0}")]
    Parse(String),
}
