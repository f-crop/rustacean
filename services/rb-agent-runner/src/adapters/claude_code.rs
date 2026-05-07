use async_trait::async_trait;
use std::path::Path;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};

use crate::adapter::{AdapterError, AdapterResult, ProcessHandle, RuntimeAdapter};

pub struct ClaudeCodeAdapter;

impl ClaudeCodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl RuntimeAdapter for ClaudeCodeAdapter {
    fn runtime_name(&self) -> &'static str {
        "claude"
    }

    async fn spawn(
        &self,
        workspace_path: &Path,
        input_prompt: &str,
        _api_key: Option<&str>,
    ) -> AdapterResult<ProcessHandle> {
        let mut child = Command::new("claude")
            .arg("--jsonl")
            .current_dir(workspace_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| AdapterError::SpawnFailed(format!("Failed to spawn claude: {}", e)))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(input_prompt.as_bytes())
                .await
                .map_err(|e| AdapterError::SpawnFailed(format!("Failed to write initial input: {}", e)))?;
            stdin
                .write_all(b"\n")
                .await
                .map_err(|e| AdapterError::SpawnFailed(format!("Failed to write newline: {}", e)))?;
        }

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
                tracing::debug!(target: "claude.stdout", "{}", line);
            }
        });

        let stderr_reader = tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "claude.stderr", "{}", line);
            }
        });

        Ok(ProcessHandle {
            child,
            stdout_reader,
            stderr_reader,
        })
    }

    async fn send_input(&self, handle: &mut ProcessHandle, input: &str) -> AdapterResult<()> {
        if let Some(mut stdin) = handle.child.stdin.take() {
            stdin
                .write_all(input.as_bytes())
                .await
                .map_err(|e| AdapterError::InputFailed(e.to_string()))?;
            stdin
                .write_all(b"\n")
                .await
                .map_err(|e| AdapterError::InputFailed(e.to_string()))?;
            handle.child.stdin = Some(stdin);
            Ok(())
        } else {
            Err(AdapterError::NotRunning)
        }
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
