use async_trait::async_trait;
use std::path::Path;

use crate::adapter::{AdapterError, AdapterResult, ProcessHandle, RuntimeAdapter};

pub struct PiAdapter;

impl PiAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl RuntimeAdapter for PiAdapter {
    fn runtime_name(&self) -> &'static str {
        "pi"
    }

    async fn spawn(
        &self,
        _workspace_path: &Path,
        _input_prompt: &str,
        _api_key: Option<&str>,
    ) -> AdapterResult<ProcessHandle> {
        Err(AdapterError::SpawnFailed(
            "PiAdapter not implemented (deferred to future wave)".to_string(),
        ))
    }

    async fn send_input(&self, _handle: &mut ProcessHandle, _input: &str) -> AdapterResult<()> {
        Err(AdapterError::NotRunning)
    }

    async fn terminate(&self, _handle: &mut ProcessHandle, _force: bool) -> AdapterResult<()> {
        Err(AdapterError::NotRunning)
    }
}
