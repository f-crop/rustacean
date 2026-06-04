use anyhow::{Context, Result};
use async_trait::async_trait;
use rb_schemas::AgentRuntime;
use tokio::io::AsyncWriteExt;

use super::{
    AgentProcess, LineKind, ParsedLine, RuntimeAdapter, RuntimeCaps, RuntimeManifest, SessionCtx,
    build_base_command, write_mcp_config,
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
    fn manifest(&self) -> RuntimeManifest {
        RuntimeManifest {
            kind: rb_schemas::AgentRuntime::ClaudeCode,
            binary: "claude",
            required_env: &["ANTHROPIC_API_KEY"],
            capabilities: RuntimeCaps {
                multi_turn: true,
                streams_json: true,
            },
        }
    }

    async fn spawn(&self, ctx: &SessionCtx) -> Result<AgentProcess> {
        // Auth mode: prefer ANTHROPIC_API_KEY (direct API key); if absent, require
        // credentials.json written by `claude /login` in the SSH sidecar (OAuth/Max mode).
        let anthropic_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
        let ro_config_dir = std::env::var("CLAUDE_CONFIG_DIR")
            .unwrap_or_else(|_| DEFAULT_CLAUDE_CONFIG_DIR.to_string());
        if anthropic_key.is_empty() {
            let creds = std::path::PathBuf::from(&ro_config_dir).join("credentials.json");
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

        // The shared credentials volume is mounted read-only to prevent the
        // spawned claude child from rewriting credentials.  Create a per-session
        // writable config directory and copy credential files into it so the CLI
        // can write session state (.claude.json) without hitting EROFS.
        let session_config = ctx.workspace_path.join(".claude-config");
        tokio::fs::create_dir_all(&session_config)
            .await
            .context("Failed to create session config dir")?;
        let ro_dir = std::path::PathBuf::from(&ro_config_dir);
        for name in [".credentials.json", "credentials.json", ".claude.json"] {
            let src = ro_dir.join(name);
            if tokio::fs::metadata(&src).await.is_ok() {
                tokio::fs::copy(&src, session_config.join(name))
                    .await
                    .with_context(|| format!("Failed to copy {name} to session config"))?;
            }
        }

        let mcp_config_path = ctx.workspace_path.join(".mcp.json");
        let mut cmd = build_base_command("claude", &ctx.workspace_path);
        cmd.args(["-p", "--output-format", "stream-json", "--verbose"])
            .arg("--dangerously-skip-permissions")
            .arg("--mcp-config")
            .arg(&mcp_config_path)
            .env("CLAUDE_CONFIG_DIR", &session_config)
            .env("RB_AGENT_API_KEY", &ctx.api_key)
            .env("RB_AGENT_TENANT_ID", &ctx.tenant_id);

        if ctx.initial_prompt.is_empty() {
            // Chat sessions: keep the process alive for multi-turn by reading NDJSON
            // turns from stdin instead of consuming a one-shot positional arg.
            cmd.arg("--input-format").arg("stream-json");
        } else {
            // Non-chat (one-shot) sessions: pass the full prompt as a CLI arg.
            // `--` terminates flag parsing so a prompt starting with `-` cannot
            // inject CLI flags into the spawned process.
            cmd.arg("--").arg(&ctx.initial_prompt);
        }

        let mut child = cmd.spawn().context("Failed to spawn claude process")?;
        let pid = child.id().context("Failed to get process ID")?;
        // Extract stdin before handing child to AgentProcess.  tokio ≥1.52
        // drops child.stdin inside Child::wait(), which would EOF Claude and
        // cause exit code 1 (RUSAA-1870).
        let stdin = child.stdin.take();

        Ok(AgentProcess {
            child: Some(child),
            pid,
            runtime: AgentRuntime::ClaudeCode,
            stdin,
        })
    }

    async fn send_input(&self, proc: &mut AgentProcess, input: &str) -> Result<()> {
        // Use as_mut() (not take()) so stdin remains open for the next turn.
        // Each turn is a JSON line in the SDK's stream-json envelope format.
        let Some(stdin) = proc.stdin.as_mut() else {
            anyhow::bail!("Process stdin not available")
        };
        let ndjson = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": input }
        })
        .to_string();
        stdin.write_all(ndjson.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
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
        if let Some(ref mut c) = proc.child {
            c.kill().await?;
        }
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
    async fn mcp_config_written_to_workspace_before_spawn() {
        let _guard = ENV_LOCK.lock().await;
        let tmp = TempDir::new().unwrap();
        // SAFETY: ENV_LOCK serializes all env mutations across these tests.
        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", "test-key");
            std::env::set_var("RUST_BRAIN_API_BASE", "http://control-api:8081");
            std::env::remove_var("MCP_SERVER_CMD");
        }

        let adapter = ClaudeCodeAdapter::new();
        // .mcp.json must be written to workspace_path before claude is invoked;
        // spawn passes --mcp-config pointing to this file.  Verify the file exists
        // regardless of whether the claude binary is present in the test environment.
        let _ = adapter.spawn(&make_ctx(tmp.path())).await;

        let mcp_path = tmp.path().join(".mcp.json");
        assert!(
            mcp_path.exists(),
            ".mcp.json must be written to workspace before spawn so --mcp-config resolves"
        );
        let raw = std::fs::read_to_string(&mcp_path).unwrap();
        let cfg: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            cfg["mcpServers"]["rust-brain"]["command"], "rustbrain-mcp",
            "MCP command must be the pre-installed binary"
        );
    }

    /// `send_input` must produce valid NDJSON using the SDK's stream-json envelope.
    /// The content field must be properly escaped (newlines, quotes, etc.).
    #[test]
    fn send_input_ndjson_envelope_is_valid() {
        let content = "Hello,\nworld! \"quoted\"";
        let ndjson = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": content }
        })
        .to_string();

        let parsed: serde_json::Value = serde_json::from_str(&ndjson).expect("must be valid JSON");
        assert_eq!(parsed["type"].as_str().unwrap(), "user");
        assert_eq!(
            parsed["message"]["role"].as_str().unwrap(),
            "user",
            "role must be 'user'"
        );
        assert_eq!(
            parsed["message"]["content"].as_str().unwrap(),
            content,
            "content must round-trip without loss"
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
