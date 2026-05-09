use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result};
use async_trait::async_trait;
use rb_schemas::AgentRuntime;
use tokio::process::{Child, Command};

pub mod claude_code;
pub mod opencode;
pub mod pi;

#[derive(Debug, Clone)]
pub struct SessionCtx {
    #[allow(dead_code)]
    pub session_id: String,
    pub tenant_id: String,
    pub workspace_path: PathBuf,
    pub api_key: String,
    pub initial_prompt: String,
}

#[derive(Debug)]
pub struct AgentProcess {
    pub child: Child,
    pub pid: u32,
    pub runtime: AgentRuntime,
}

#[async_trait]
pub trait RuntimeAdapter: Send + Sync {
    async fn spawn(&self, ctx: &SessionCtx) -> Result<AgentProcess>;
    async fn send_input(&self, proc: &mut AgentProcess, input: &str) -> Result<()>;
    async fn terminate(&self, proc: &mut AgentProcess, force: bool) -> Result<()>;
    fn parse_stdout_line(&self, line: &str) -> Option<ParsedLine>;
}

#[derive(Debug, Clone)]
pub struct ParsedLine {
    #[allow(dead_code)]
    pub kind: LineKind,
    pub payload: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Text,
    Json,
}

pub fn adapter_for_runtime(runtime: AgentRuntime) -> anyhow::Result<Box<dyn RuntimeAdapter>> {
    match runtime {
        AgentRuntime::ClaudeCode => Ok(Box::new(claude_code::ClaudeCodeAdapter::new())),
        AgentRuntime::Opencode => Ok(Box::new(opencode::OpencodeAdapter::new())),
        AgentRuntime::Pi => Ok(Box::new(pi::PiAdapter::new())),
        AgentRuntime::Unspecified => anyhow::bail!("Unspecified runtime received"),
    }
}

pub(crate) fn build_base_command(binary: &str, workspace: &PathBuf) -> Command {
    let mut cmd = Command::new(binary);
    cmd.current_dir(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd
}

pub(crate) async fn check_binary(binary: &str) -> Result<()> {
    match Command::new("which").arg(binary).output().await {
        Ok(output) if output.status.success() => Ok(()),
        _ => anyhow::bail!("Binary '{binary}' not found in PATH"),
    }
}

pub async fn write_mcp_config(
    workspace: &std::path::Path,
    _api_key: &str,
    tenant_id: &str,
) -> Result<()> {
    // C3: Do not write API key to disk - use environment variable reference instead
    // The actual API key should be passed via RB_AGENT_API_KEY environment variable
    // set by the adapter when spawning the process, not written to config files
    let api_base = std::env::var("RUST_BRAIN_API_BASE")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());

    // C4: Validate URL scheme to prevent SSRF attacks
    if !api_base.starts_with("http://") && !api_base.starts_with("https://") {
        anyhow::bail!("RUST_BRAIN_API_BASE must use http:// or https:// scheme, got: {api_base}",);
    }

    let mcp_config = serde_json::json!({
        "mcpServers": {
            "rust-brain": {
                "command": "npx",
                "args": ["-y", "@modelcontextprotocol/server-rust-brain"],
                "env": {
                    "RUST_BRAIN_API_KEY": "${RUST_BRAIN_API_KEY}",
                    "RUST_BRAIN_TENANT_ID": tenant_id,
                    "RUST_BRAIN_API_BASE": api_base
                }
            }
        }
    });

    let mcp_path = workspace.join(".mcp.json");
    tokio::fs::write(&mcp_path, serde_json::to_string_pretty(&mcp_config)?)
        .await
        .with_context(|| format!("Failed to write .mcp.json to {}", mcp_path.display()))?;

    // C2: Set restrictive permissions (0600) to prevent world-readable API keys
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        tokio::fs::set_permissions(&mcp_path, perms)
            .await
            .with_context(|| format!("Failed to set 0600 permissions on {}", mcp_path.display()))?;
    }

    Ok(())
}
