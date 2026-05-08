use std::collections::HashMap;

use anyhow::{Context, Result};
use async_trait::async_trait;
use rb_schemas::AgentRuntime;
use tokio::io::AsyncWriteExt;

use super::{
    AgentProcess, LineKind, ParsedLine, RuntimeAdapter, SessionCtx, build_base_command,
    check_binary, write_mcp_config,
};

pub struct OpencodeAdapter {
    default_provider: String,
    default_model: String,
}

impl OpencodeAdapter {
    pub fn new() -> Self {
        Self {
            default_provider: std::env::var("OPENCODE_DEFAULT_PROVIDER")
                .unwrap_or_else(|_| "anthropic".to_string()),
            default_model: std::env::var("OPENCODE_DEFAULT_MODEL")
                .unwrap_or_else(|_| "claude-sonnet-4-20250514".to_string()),
        }
    }

    fn write_opencode_config(&self, workspace: &std::path::Path) -> Result<()> {
        let config = serde_json::json!({
            "provider": self.default_provider,
            "model": self.default_model,
        });
        let opencode_dir = workspace.join(".opencode");
        std::fs::create_dir_all(&opencode_dir)?;
        let config_path = opencode_dir.join("config.json");
        std::fs::write(&config_path, serde_json::to_string_pretty(&config)?).with_context(
            || {
                format!(
                    "Failed to write opencode config to {}",
                    config_path.display()
                )
            },
        )?;
        Ok(())
    }

    fn collect_provider_env() -> HashMap<String, String> {
        let mut env_vars = HashMap::new();
        for key in &[
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "GOOGLE_API_KEY",
            "GROQ_API_KEY",
            "MISTRAL_API_KEY",
        ] {
            if let Ok(val) = std::env::var(key) {
                env_vars.insert((*key).to_string(), val);
            }
        }
        if let Ok(api_base) = std::env::var("OPENCODE_API_BASE") {
            env_vars.insert("OPENCODE_API_BASE".to_string(), api_base);
        }
        env_vars
    }
}

#[async_trait]
impl RuntimeAdapter for OpencodeAdapter {
    async fn spawn(&self, ctx: &SessionCtx) -> Result<AgentProcess> {
        check_binary("opencode").await?;
        write_mcp_config(&ctx.workspace_path, &ctx.api_key, &ctx.tenant_id)
            .context("Failed to write MCP config")?;
        self.write_opencode_config(&ctx.workspace_path)
            .context("Failed to write opencode config")?;

        let mut cmd = build_base_command("opencode", &ctx.workspace_path);
        cmd.env("RB_AGENT_API_KEY", &ctx.api_key)
            .env("RB_AGENT_TENANT_ID", &ctx.tenant_id);

        for (key, val) in Self::collect_provider_env() {
            cmd.env(key, val);
        }

        if !ctx.initial_prompt.is_empty() {
            // `--` terminates flag parsing so a prompt starting with `-` cannot
            // inject CLI flags into the spawned process.
            cmd.args(["run", "--", &ctx.initial_prompt]);
        }

        let child = cmd.spawn().context("Failed to spawn opencode process")?;
        let pid = child.id().context("Failed to get process ID")?;

        Ok(AgentProcess {
            child,
            pid,
            runtime: AgentRuntime::Opencode,
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
            // Linux PIDs are constrained to fit in i32; use fallback on overflow.
            let pid_i32 = i32::try_from(proc.pid).unwrap_or(i32::MAX);
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
