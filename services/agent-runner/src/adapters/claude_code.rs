use anyhow::{Context, Result};
use async_trait::async_trait;
use rb_schemas::AgentRuntime;
use tokio::io::AsyncWriteExt;

use super::{
    AgentProcess, LineKind, ParsedLine, RuntimeAdapter, SessionCtx, build_base_command,
    write_mcp_config,
};

const DEFAULT_CLAUDE_CONFIG_DIR: &str = "/home/loginuser/.claude";

pub struct ClaudeCodeAdapter;

impl ClaudeCodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl RuntimeAdapter for ClaudeCodeAdapter {
    async fn spawn(&self, ctx: &SessionCtx) -> Result<AgentProcess> {
        // Auth mode: prefer ANTHROPIC_API_KEY (direct API key); if absent, require
        // credentials.json written by `claude /login` in the SSH sidecar (OAuth/Max mode).
        let anthropic_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
        if anthropic_key.is_empty() {
            let config_dir = std::env::var("CLAUDE_CONFIG_DIR")
                .unwrap_or_else(|_| DEFAULT_CLAUDE_CONFIG_DIR.to_string());
            let creds = std::path::PathBuf::from(&config_dir).join("credentials.json");
            if !creds.exists() {
                anyhow::bail!(
                    "claude_not_logged_in: {} not found. \
                     SSH into the application via port 12222 and run `claude /login`.",
                    creds.display()
                );
            }
        }

        write_mcp_config(&ctx.workspace_path, &ctx.api_key, &ctx.tenant_id)
            .await
            .context("Failed to write MCP config")?;

        let mut cmd = build_base_command("claude", &ctx.workspace_path);
        cmd.args(["-p", "--output-format", "stream-json"])
            .arg("--dangerously-skip-permissions")
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    // Serialize tests that mutate process environment variables.
    static ENV_LOCK: Mutex<()> = Mutex::const_new(());

    fn make_ctx(workspace: &std::path::Path) -> SessionCtx {
        SessionCtx {
            session_id: "test-session".to_string(),
            tenant_id: "test-tenant".to_string(),
            workspace_path: workspace.to_path_buf(),
            api_key: "test-key".to_string(),
            initial_prompt: String::new(),
        }
    }

    #[tokio::test]
    async fn spawn_fails_when_credentials_missing() {
        let _guard = ENV_LOCK.lock().await;
        let tmp = TempDir::new().unwrap();
        // SAFETY: ENV_LOCK serializes all env mutations across these tests.
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::set_var("CLAUDE_CONFIG_DIR", tmp.path());
        }
        // No credentials.json exists in the fresh tempdir.

        let adapter = ClaudeCodeAdapter::new();
        let err = adapter.spawn(&make_ctx(tmp.path())).await.unwrap_err();
        assert!(
            err.to_string().contains("claude_not_logged_in"),
            "expected claude_not_logged_in, got: {err}"
        );
    }

    #[tokio::test]
    async fn spawn_proceeds_past_preflight_when_credentials_present() {
        let _guard = ENV_LOCK.lock().await;
        let tmp = TempDir::new().unwrap();
        // SAFETY: ENV_LOCK serializes all env mutations across these tests.
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::set_var("CLAUDE_CONFIG_DIR", tmp.path());
        }
        std::fs::write(tmp.path().join("credentials.json"), r#"{"stub":true}"#).unwrap();

        let adapter = ClaudeCodeAdapter::new();
        // Preflight passed when credentials exist.  The spawn may succeed (claude binary
        // present) or fail for an unrelated reason — what must NOT happen is a
        // claude_not_logged_in error.
        match adapter.spawn(&make_ctx(tmp.path())).await {
            Ok(_) => {}
            Err(e) => assert!(
                !e.to_string().contains("claude_not_logged_in"),
                "should not be a credentials error, got: {e}"
            ),
        }
    }
}
