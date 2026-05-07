//! Runtime error types for rb-agent-runtime.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("OAuth token missing or expired for runtime '{runtime_kind}'")]
    TokenMissing { runtime_kind: String },

    #[error("Anthropic API error (status={status}): {message}")]
    AnthropicApi { status: u16, message: String },

    #[error("LiteLLM API error (status={status}): {message}")]
    LiteLlmApi { status: u16, message: String },

    #[error("token budget exhausted (used={used}, budget={budget})")]
    BudgetExhausted { used: i64, budget: i64 },

    #[error("session was cancelled")]
    Cancelled,

    #[error("tool dispatch error: {0}")]
    ToolDispatch(String),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("HTTP client error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("internal error: {0}")]
    Internal(String),
}

impl RuntimeError {
    pub fn internal(msg: impl Into<String>) -> Self {
        RuntimeError::Internal(msg.into())
    }

    pub fn tool_dispatch(msg: impl Into<String>) -> Self {
        RuntimeError::ToolDispatch(msg.into())
    }
}
