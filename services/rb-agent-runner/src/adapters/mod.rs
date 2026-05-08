use std::process::Stdio;

use rb_schemas::RuntimeKind;
use tokio::process::Command;
use uuid::Uuid;

use crate::error::{Result, RunnerError};
use crate::workspace::Workspace;

pub fn runtime_kind_as_str(kind: RuntimeKind) -> &'static str {
    match kind {
        RuntimeKind::Unspecified => "unspecified",
        RuntimeKind::ClaudeCode => "claude_code",
        RuntimeKind::Opcode => "opcode",
        RuntimeKind::Pi => "pi",
    }
}

#[async_trait::async_trait]
pub trait RuntimeAdapter: Send + Sync + 'static {
    fn runtime_kind(&self) -> RuntimeKind;

    async fn spawn(
        &self,
        workspace: &Workspace,
        session_id: Uuid,
        api_key: &str,
    ) -> Result<tokio::process::Child>;
}

pub struct ClaudeCodeConfig {
    pub binary_path: String,
}

impl Default for ClaudeCodeConfig {
    fn default() -> Self {
        Self {
            binary_path: "claude".to_string(),
        }
    }
}

pub struct ClaudeCodeAdapter {
    config: ClaudeCodeConfig,
}

impl ClaudeCodeAdapter {
    pub fn new(config: ClaudeCodeConfig) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl RuntimeAdapter for ClaudeCodeAdapter {
    fn runtime_kind(&self) -> RuntimeKind {
        RuntimeKind::ClaudeCode
    }

    async fn spawn(
        &self,
        workspace: &Workspace,
        session_id: Uuid,
        api_key: &str,
    ) -> Result<tokio::process::Child> {
        let child = Command::new(&self.config.binary_path)
            .arg("--session")
            .arg(session_id.to_string())
            .current_dir(&workspace.path)
            .env("RB_AGENT_API_KEY", api_key)
            .env("RB_SESSION_ID", session_id.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| RunnerError::ProcessSpawn(format!("claude: {e}")))?;

        tracing::info!(
            session_id = %session_id,
            pid = child.id().unwrap_or(0),
            "spawned claude process"
        );

        Ok(child)
    }
}

pub struct OpencodeConfig {
    pub binary_path: String,
}

impl Default for OpencodeConfig {
    fn default() -> Self {
        Self {
            binary_path: "opencode".to_string(),
        }
    }
}

pub struct OpencodeAdapter {
    config: OpencodeConfig,
}

impl OpencodeAdapter {
    pub fn new(config: OpencodeConfig) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl RuntimeAdapter for OpencodeAdapter {
    fn runtime_kind(&self) -> RuntimeKind {
        RuntimeKind::Opcode
    }

    async fn spawn(
        &self,
        workspace: &Workspace,
        session_id: Uuid,
        api_key: &str,
    ) -> Result<tokio::process::Child> {
        let opencode_config = serde_json::json!({
            "session_id": session_id.to_string(),
            "api_key": api_key,
            "workspace_root": workspace.path.to_string_lossy(),
        });
        workspace.write_file(".opencode/config.json", opencode_config.to_string().as_bytes()).await?;

        let mcp_config = serde_json::json!({
            "mcpServers": {
                "rustbrain": {
                    "command": "none",
                    "args": [],
                    "env": {
                        "RB_AGENT_API_KEY": api_key,
                        "RB_SESSION_ID": session_id.to_string()
                    }
                }
            }
        });
        workspace.write_file(".mcp.json", mcp_config.to_string().as_bytes()).await?;

        let child = Command::new(&self.config.binary_path)
            .current_dir(&workspace.path)
            .env("RB_AGENT_API_KEY", api_key)
            .env("RB_SESSION_ID", session_id.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| RunnerError::ProcessSpawn(format!("opencode: {e}")))?;

        tracing::info!(
            session_id = %session_id,
            pid = child.id().unwrap_or(0),
            "spawned opencode process"
        );

        Ok(child)
    }
}

pub struct PiAdapter;

impl PiAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl RuntimeAdapter for PiAdapter {
    fn runtime_kind(&self) -> RuntimeKind {
        RuntimeKind::Pi
    }

    async fn spawn(
        &self,
        _workspace: &Workspace,
        _session_id: Uuid,
        _api_key: &str,
    ) -> Result<tokio::process::Child> {
        Err(RunnerError::NotImplemented("pi".to_string()))
    }
}

pub struct AdapterFactory;

impl AdapterFactory {
    pub fn create(kind: RuntimeKind) -> Result<Box<dyn RuntimeAdapter>> {
        match kind {
            RuntimeKind::ClaudeCode => Ok(Box::new(ClaudeCodeAdapter::new(ClaudeCodeConfig::default()))),
            RuntimeKind::Opcode => Ok(Box::new(OpencodeAdapter::new(OpencodeConfig::default()))),
            RuntimeKind::Pi => Ok(Box::new(PiAdapter::new())),
            RuntimeKind::Unspecified => Err(RunnerError::InvalidRuntimeKind("unspecified".to_string())),
        }
    }
}
