use anyhow::{Context, Result};
use async_trait::async_trait;
use rb_schemas::AgentRuntime;
use tokio::io::AsyncWriteExt;

use super::{
    AgentProcess, LineKind, ParsedLine, RuntimeAdapter, SessionCtx, build_base_command,
    check_binary, write_mcp_config,
};

pub struct ClaudeCodeAdapter;

impl ClaudeCodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl RuntimeAdapter for ClaudeCodeAdapter {
    async fn spawn(&self, ctx: &SessionCtx) -> Result<AgentProcess> {
        check_binary("claude").await?;
        write_mcp_config(&ctx.workspace_path, &ctx.api_key, &ctx.tenant_id)
            .await
            .context("Failed to write MCP config")?;

        let mut cmd = build_base_command("claude", &ctx.workspace_path);
        cmd.arg("--jsonl")
            .env("RB_AGENT_API_KEY", &ctx.api_key)
            .env("RB_AGENT_TENANT_ID", &ctx.tenant_id);

        if !ctx.initial_prompt.is_empty() {
            // `--` terminates flag parsing so a prompt starting with `-` cannot
            // inject CLI flags into the spawned process.
            cmd.arg("--").arg(&ctx.initial_prompt);
        }

        let child = cmd.spawn().context("Failed to spawn claude process")?;
        let pid = child.id().context("Failed to get process ID")?;

        Ok(AgentProcess {
            child,
            pid,
            runtime: AgentRuntime::ClaudeCode,
        })
    }

    async fn send_input(&self, proc: &mut AgentProcess, input: &str) -> Result<()> {
        if let Some(stdin) = proc.child.stdin.as_mut() {
            stdin.write_all(input.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;
            Ok(())
        } else {
            anyhow::bail!("Process stdin not available")
        }
    }

    async fn terminate(&self, proc: &mut AgentProcess, force: bool) -> Result<()> {
        #[cfg(unix)]
        {
            use nix::sys::signal::{Signal, kill};
            use nix::unistd::Pid;
            let signal = if force {
                Signal::SIGKILL
            } else {
                Signal::SIGTERM
            };
            // H3: Never fallback to i32::MAX — would send signal to wrong process.
            // Linux PIDs are constrained to fit in i32; overflow is impossible on valid systems.
            let pid_i32 = i32::try_from(proc.pid)
                .map_err(|_| anyhow::anyhow!("PID {} exceeds i32 range", proc.pid))?;
            kill(Pid::from_raw(pid_i32), signal).context("Failed to send signal")?;
        }
        #[cfg(not(unix))]
        proc.child.kill().await?;
        Ok(())
    }

    fn parse_stdout_line(&self, line: &str) -> Option<ParsedLine> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        if trimmed.starts_with('{') {
            Some(ParsedLine {
                kind: LineKind::Json,
                payload: trimmed.to_string(),
            })
        } else {
            Some(ParsedLine {
                kind: LineKind::Text,
                payload: line.to_string(),
            })
        }
    }
}
