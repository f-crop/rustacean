use async_trait::async_trait;
use std::path::Path;
use tokio::process::{Child, Command};

use crate::adapter::{AdapterError, AdapterResult, ProcessHandle, RuntimeAdapter};
use crate::config::AdapterConfig;

pub struct OpencodeAdapter {
    config: AdapterConfig,
}

impl OpencodeAdapter {
    pub fn new(config: AdapterConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl RuntimeAdapter for OpencodeAdapter {
    fn runtime_name(&self) -> &'static str {
        "opencode"
    }

    async fn spawn(
        &self,
        workspace_path: &Path,
        input_prompt: &str,
        _api_key: Option<&str>,
    ) -> AdapterResult<ProcessHandle> {
        let env_vars = self.config.env_vars_for_runtime(rb_schemas::AgentRuntime::Opencode);

        let mut cmd = Command::new("opencode");
        cmd.arg("run")
            .arg(input_prompt)
            .current_dir(workspace_path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        for (key, value) in env_vars {
            cmd.env(key, value);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| AdapterError::SpawnFailed(format!("Failed to spawn opencode: {}", e)))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AdapterError::SpawnFailed("Failed to capture stdout".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| AdapterError::SpawnFailed("Failed to capture stderr".to_string()))?;

        let stdout_reader = tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "opencode.stdout", "{}", line);
            }
        });

        let stderr_reader = tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "opencode.stderr", "{}", line);
            }
        });

        Ok(ProcessHandle {
            child,
            stdout_reader,
            stderr_reader,
        })
    }

    async fn send_input(&self, _handle: &mut ProcessHandle, _input: &str) -> AdapterResult<()> {
        tracing::warn!("OpencodeAdapter::send_input called but opencode does not support stdin input");
        Ok(())
    }

    async fn terminate(&self, handle: &mut ProcessHandle, force: bool) -> AdapterResult<()> {
        if force {
            handle
                .child
                .kill()
                .await
                .map_err(|e| AdapterError::Io(e))?;
        } else {
            handle
                .child
                .wait()
                .await
                .map_err(|e| AdapterError::Io(e))?;
        }
        handle.stdout_reader.abort();
        handle.stderr_reader.abort();
        Ok(())
    }
}
