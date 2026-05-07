use async_trait::async_trait;
use std::path::Path;
use thiserror::Error;
use tokio::process::Child;

#[derive(Error, Debug)]
pub enum AdapterError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Process spawn failed: {0}")]
    SpawnFailed(String),
    #[error("Process not running")]
    NotRunning,
    #[error("JSON parse error: {0}")]
    JsonParse(#[from] serde_json::Error),
    #[error("Input failed: {0}")]
    InputFailed(String),
}

pub type AdapterResult<T> = Result<T, AdapterError>;

pub struct ProcessHandle {
    pub child: Child,
    pub stdout_reader: tokio::task::JoinHandle<()>,
    pub stderr_reader: tokio::task::JoinHandle<()>,
}

#[async_trait]
pub trait RuntimeAdapter: Send + Sync {
    fn runtime_name(&self) -> &'static str;

    async fn spawn(
        &self,
        workspace_path: &Path,
        input_prompt: &str,
        api_key: Option<&str>,
    ) -> AdapterResult<ProcessHandle>;

    async fn send_input(&self, handle: &mut ProcessHandle, input: &str) -> AdapterResult<()>;

    async fn terminate(&self, handle: &mut ProcessHandle, force: bool) -> AdapterResult<()>;
}
