//! Error types for `rb-agent-runner`.

use thiserror::Error;

/// Top-level error type for agent runner operations.
#[derive(Error, Debug)]
pub enum RunnerError {
    /// Kafka consumer/producer error.
    #[error("kafka error: {0}")]
    Kafka(#[from] rb_kafka::KafkaError),

    /// IO error during workspace operations.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Process spawning failed.
    #[error("failed to spawn process: {0}")]
    ProcessSpawn(String),

    /// Process execution failed.
    #[allow(dead_code)]
    #[error("process execution failed: {0}")]
    ProcessExecution(String),

    /// Invalid runtime kind requested.
    #[error("invalid runtime kind: {0}")]
    InvalidRuntimeKind(String),

    /// Workspace initialization failed.
    #[allow(dead_code)]
    #[error("workspace initialization failed: {0}")]
    WorkspaceInit(String),

    /// Session not found.
    #[allow(dead_code)]
    #[error("session not found: {0}")]
    SessionNotFound(uuid::Uuid),

    /// Runtime not implemented.
    #[error("runtime '{0}' is not implemented")]
    NotImplemented(String),

    /// Configuration error.
    #[allow(dead_code)]
    #[error("configuration error: {0}")]
    Config(String),

    /// Serialization error.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// UUID parsing error.
    #[error("uuid error: {0}")]
    Uuid(#[from] uuid::Error),
}

/// Result type alias for `RunnerError`.
pub type Result<T> = std::result::Result<T, RunnerError>;
