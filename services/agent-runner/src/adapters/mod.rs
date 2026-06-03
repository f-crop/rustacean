use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result};
use async_trait::async_trait;
use rb_schemas::AgentRuntime;
use tokio::process::{Child, ChildStdin, Command};

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
    /// Stdin extracted from `child` at spawn time.  tokio ≥1.52 drops
    /// `child.stdin` inside `Child::wait()`, which would send EOF to the
    /// subprocess and cause Claude to exit code 1 (RUSAA-1870).  Extracting
    /// it here prevents `wait()` from closing the pipe while `send_input` is
    /// still writing to it.
    pub stdin: Option<ChildStdin>,
}

/// Static description of a runtime: used for registry validation and docs generation (ADR-013 §4.1).
#[derive(Debug, Clone)]
pub struct RuntimeManifest {
    pub kind: AgentRuntime,
    pub binary: &'static str,
    /// Env vars that must be present for the runtime to function.
    /// Read by S7 docs generation and future health checks; not yet consumed at startup.
    #[allow(dead_code)]
    pub required_env: &'static [&'static str],
    pub capabilities: RuntimeCaps,
}

/// Supported capabilities declared by a runtime adapter.
#[derive(Debug, Clone, Copy)]
pub struct RuntimeCaps {
    /// The runtime accepts multiple user turns over stdin within one session.
    pub multi_turn: bool,
    /// The runtime emits newline-delimited JSON on stdout.
    pub streams_json: bool,
}

/// Result of the `health` liveness probe (ADR-013 §4.1).
/// Not called by the binary yet — the idle reaper is future work.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeHealth {
    Alive,
    Dead,
}

#[async_trait]
pub trait RuntimeAdapter: Send + Sync {
    /// Static description of the runtime (binary, env deps, capabilities).
    /// Drives registry validation — a new runtime adds exactly one impl + one registry entry.
    fn manifest(&self) -> RuntimeManifest;

    /// Spawn one supervised OS process for a session in an isolated workspace.
    async fn spawn(&self, ctx: &SessionCtx) -> Result<AgentProcess>;

    /// Feed one user turn to a live process over stdin.
    async fn send_input(&self, proc: &mut AgentProcess, input: &str) -> Result<()>;

    /// Graceful (SIGTERM) then forced (SIGKILL) termination.
    async fn terminate(&self, proc: &mut AgentProcess, force: bool) -> Result<()>;

    /// Parse one stdout line into a typed event.
    fn parse_stdout_line(&self, line: &str) -> Option<ParsedLine>;

    /// Liveness probe used by the idle/health reaper (ADR-013 §4.1).
    /// Uses signal-0 on Unix: returns `Alive` if the OS process exists, `Dead` otherwise.
    /// Not called by the binary yet — the idle reaper is future work.
    #[allow(dead_code)]
    async fn health(&self, proc: &AgentProcess) -> RuntimeHealth {
        #[cfg(unix)]
        {
            use nix::sys::signal::kill;
            use nix::unistd::Pid;
            match i32::try_from(proc.pid) {
                Ok(pid_i32) => match kill(Pid::from_raw(pid_i32), None::<nix::sys::signal::Signal>)
                {
                    Ok(()) => RuntimeHealth::Alive,
                    Err(_) => RuntimeHealth::Dead,
                },
                Err(_) => RuntimeHealth::Dead,
            }
        }
        #[cfg(not(unix))]
        {
            let _ = proc;
            RuntimeHealth::Alive
        }
    }
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

#[cfg(test)]
mod trait_tests {
    use super::*;

    #[test]
    fn claude_code_manifest_fields() {
        let m = claude_code::ClaudeCodeAdapter::new().manifest();
        assert_eq!(m.kind, AgentRuntime::ClaudeCode);
        assert_eq!(m.binary, "claude");
        assert!(m.capabilities.multi_turn);
        assert!(m.capabilities.streams_json);
    }

    #[test]
    fn opencode_manifest_fields() {
        let m = opencode::OpencodeAdapter::new().manifest();
        assert_eq!(m.kind, AgentRuntime::Opencode);
        assert_eq!(m.binary, "opencode");
    }

    #[test]
    fn pi_manifest_fields() {
        let m = pi::PiAdapter::new().manifest();
        assert_eq!(m.kind, AgentRuntime::Pi);
        assert_eq!(m.binary, "pi");
    }

    #[test]
    fn runtime_health_variants_are_distinct() {
        assert_ne!(RuntimeHealth::Alive, RuntimeHealth::Dead);
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

pub async fn write_mcp_config(
    workspace: &std::path::Path,
    api_key: &str,
    tenant_id: &str,
) -> Result<()> {
    // Prefer an explicit override; fall back to the control-api base URL that
    // compose already sets as RB_CONTROL_API_BASE_URL; last resort: localhost.
    let api_base = std::env::var("RUST_BRAIN_API_BASE")
        .or_else(|_| std::env::var("RB_CONTROL_API_BASE_URL"))
        .unwrap_or_else(|_| "http://localhost:8080".to_string());

    // C4: Validate URL scheme to prevent SSRF attacks
    if !api_base.starts_with("http://") && !api_base.starts_with("https://") {
        anyhow::bail!("RUST_BRAIN_API_BASE must use http:// or https:// scheme, got: {api_base}");
    }

    // Use the binary pre-installed in the Docker image (rustbrain-mcp) so spawned
    // sessions don't need npm-registry access. MCP_SERVER_CMD lets local dev
    // or tests override the path without rebuilding the image.
    let mcp_cmd = std::env::var("MCP_SERVER_CMD").unwrap_or_else(|_| "rustbrain-mcp".to_string());

    let mcp_config = serde_json::json!({
        "mcpServers": {
            "rust-brain": {
                "command": mcp_cmd,
                "args": [],
                "env": {
                    "RB_AGENT_API_KEY": api_key,
                    "RB_AGENT_TENANT_ID": tenant_id,
                    "RB_AGENT_API_BASE": api_base
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    // Serialize tests that mutate process environment variables.
    static ENV_LOCK: Mutex<()> = Mutex::const_new(());

    #[tokio::test]
    async fn write_mcp_config_falls_back_to_rb_control_api_base_url() {
        let _guard = ENV_LOCK.lock().await;
        let tmp = TempDir::new().unwrap();
        // SAFETY: ENV_LOCK serializes all env mutations in this module's tests.
        unsafe {
            std::env::remove_var("RUST_BRAIN_API_BASE");
            std::env::set_var("RB_CONTROL_API_BASE_URL", "http://control-api:8081");
            std::env::remove_var("MCP_SERVER_CMD");
        }

        write_mcp_config(tmp.path(), "rb_live_test", "tenant-abc")
            .await
            .unwrap();

        let raw = tokio::fs::read_to_string(tmp.path().join(".mcp.json"))
            .await
            .unwrap();
        let cfg: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let server = &cfg["mcpServers"]["rust-brain"];
        assert_eq!(
            server["command"], "rustbrain-mcp",
            "should use pre-installed binary"
        );
        assert_eq!(
            server["args"],
            serde_json::json!([]),
            "args must be empty array"
        );
        assert_eq!(
            server["env"]["RB_AGENT_API_BASE"], "http://control-api:8081",
            "should pick up RB_CONTROL_API_BASE_URL"
        );
    }

    #[tokio::test]
    async fn write_mcp_config_rust_brain_api_base_takes_priority() {
        let _guard = ENV_LOCK.lock().await;
        let tmp = TempDir::new().unwrap();
        // SAFETY: ENV_LOCK serializes all env mutations in this module's tests.
        unsafe {
            std::env::set_var("RUST_BRAIN_API_BASE", "https://explicit.host/api");
            std::env::set_var("RB_CONTROL_API_BASE_URL", "http://control-api:8081");
            std::env::remove_var("MCP_SERVER_CMD");
        }

        write_mcp_config(tmp.path(), "rb_live_test", "tenant-abc")
            .await
            .unwrap();

        let raw = tokio::fs::read_to_string(tmp.path().join(".mcp.json"))
            .await
            .unwrap();
        let cfg: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            cfg["mcpServers"]["rust-brain"]["env"]["RB_AGENT_API_BASE"],
            "https://explicit.host/api",
            "RUST_BRAIN_API_BASE must take priority over RB_CONTROL_API_BASE_URL"
        );
    }

    #[tokio::test]
    async fn write_mcp_config_mcp_server_cmd_override() {
        let _guard = ENV_LOCK.lock().await;
        let tmp = TempDir::new().unwrap();
        // SAFETY: ENV_LOCK serializes all env mutations in this module's tests.
        unsafe {
            std::env::set_var("RUST_BRAIN_API_BASE", "http://localhost:8080");
            std::env::set_var("MCP_SERVER_CMD", "/custom/rustbrain-mcp");
        }

        write_mcp_config(tmp.path(), "rb_live_test", "tenant-abc")
            .await
            .unwrap();

        let raw = tokio::fs::read_to_string(tmp.path().join(".mcp.json"))
            .await
            .unwrap();
        let cfg: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            cfg["mcpServers"]["rust-brain"]["command"], "/custom/rustbrain-mcp",
            "MCP_SERVER_CMD must override the default binary name"
        );
    }

    #[tokio::test]
    async fn write_mcp_config_rejects_non_http_scheme() {
        let _guard = ENV_LOCK.lock().await;
        let tmp = TempDir::new().unwrap();
        // SAFETY: ENV_LOCK serializes all env mutations in this module's tests.
        unsafe {
            std::env::set_var("RUST_BRAIN_API_BASE", "file:///etc/passwd");
            std::env::remove_var("RB_CONTROL_API_BASE_URL");
        }

        let err = write_mcp_config(tmp.path(), "key", "tenant")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("must use http://"),
            "expected SSRF validation error, got: {err}"
        );
    }

    /// Regression test for RUSAA-1870: tokio ≥1.52 `Child::wait()` drops
    /// `child.stdin`, causing Claude to read EOF and exit 1.  Extracting stdin
    /// into `AgentProcess::stdin` before `wait()` prevents the close.
    #[tokio::test]
    async fn extracted_stdin_survives_child_wait() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut cmd = tokio::process::Command::new("/bin/cat");
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd.spawn().expect("/bin/cat must be available");
        let mut stdout = child.stdout.take().unwrap();
        // Simulate the AgentProcess::spawn pattern: extract stdin before wait().
        let mut stdin = child.stdin.take().expect("stdin must be piped");

        // child.stdin is now None — tokio's wait() cannot drop our handle.
        let wait_task = tokio::spawn(async move { child.wait().await });

        // Write while the wait task is live — must not fail (RUSAA-1870).
        stdin
            .write_all(b"hello\n")
            .await
            .expect("write to extracted stdin must succeed");
        drop(stdin); // close → cat exits naturally

        let mut buf = String::new();
        stdout.read_to_string(&mut buf).await.unwrap();
        assert_eq!(
            buf, "hello\n",
            "cat must echo the line written after wait started"
        );

        let status = wait_task.await.unwrap().unwrap();
        assert_eq!(status.code(), Some(0), "cat must exit 0 after stdin closes");
    }
}
