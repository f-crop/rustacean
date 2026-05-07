use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Context, Result};
use dashmap::DashMap;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{info, warn};
use uuid::Uuid;

use crate::AgentCommand;

pub struct SessionManager {
    workspace_base: String,
    sessions: DashMap<Uuid, SessionHandle>,
}

#[derive(Clone)]
#[allow(dead_code)]
struct SessionHandle {
    tenant_id: Uuid,
    child: Arc<Mutex<Option<Child>>>,
    api_key: String,
    runtime_kind: String,
}

impl SessionManager {
    pub async fn new(workspace_base: &str) -> Result<Self> {
        tokio::fs::create_dir_all(workspace_base)
            .await
            .context("failed to create workspace base directory")?;

        Ok(Self {
            workspace_base: workspace_base.to_owned(),
            sessions: DashMap::new(),
        })
    }

    pub async fn start_session(
        &self,
        session_id: Uuid,
        cmd: &AgentCommand,
    ) -> Result<()> {
        let workspace_path = format!("{}/{}/{}", self.workspace_base, cmd.tenant_id, session_id);

        tokio::fs::create_dir_all(&workspace_path)
            .await
            .context("failed to create session workspace")?;

        self.write_mcp_config(&workspace_path).await?;

        if cmd.runtime_kind == "opencode" {
            self.write_opencode_config(&workspace_path, cmd).await?;
        }

        let api_key = generate_session_api_key(session_id, cmd.tenant_id);

        let (program, args) = build_runtime_command(&cmd.runtime_kind, cmd)?;

        let child = Command::new(program)
            .args(args)
            .current_dir(&workspace_path)
            .env("RB_AGENT_API_KEY", &api_key)
            .env("RB_SESSION_ID", session_id.to_string())
            .env("RB_TENANT_ID", cmd.tenant_id.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn agent runtime process")?;

        info!(
            session_id = %session_id,
            runtime_kind = %cmd.runtime_kind,
            pid = ?child.id(),
            "agent session started"
        );

        self.sessions.insert(
            session_id,
            SessionHandle {
                tenant_id: cmd.tenant_id,
                child: Arc::new(Mutex::new(Some(child))),
                api_key,
                runtime_kind: cmd.runtime_kind.clone(),
            },
        );

        let sessions = self.sessions.clone();
        tokio::spawn(async move {
            if let Some((_, handle)) = sessions.remove(&session_id) {
                let mut child_lock = handle.child.lock().await;
                if let Some(mut child) = child_lock.take() {
                    match child.wait().await {
                        Ok(status) => {
                            let exit_code = status.code().unwrap_or(-1);
                            info!(
                                session_id = %session_id,
                                exit_code = exit_code,
                                "agent session exited"
                            );
                        }
                        Err(e) => {
                            warn!(
                                session_id = %session_id,
                                error = %e,
                                "failed to wait for child process"
                            );
                        }
                    }
                }
            }
        });

        Ok(())
    }

    pub async fn terminate_session(&self, session_id: Uuid) -> Result<i32> {
        let handle = self
            .sessions
            .get(&session_id)
            .context("session not found")?;

        let tenant_id = handle.tenant_id;

        revoke_session_api_key(session_id, tenant_id);

        let mut child_lock = handle.child.lock().await;
        if let Some(mut child) = child_lock.take() {
            #[cfg(unix)]
            {
                if let Some(pid) = child.id() {
                    if let Ok(pid_i32) = i32::try_from(pid) {
                        tokio::spawn(async move {
                            let _ = nix::sys::signal::kill(
                                nix::unistd::Pid::from_raw(pid_i32),
                                nix::sys::signal::Signal::SIGTERM,
                            );
                        });
                    }
                }
            }

            match tokio::time::timeout(tokio::time::Duration::from_secs(10), child.wait()).await {
                Ok(Ok(status)) => {
                    let exit_code = status.code().unwrap_or(0);
                    info!(
                        session_id = %session_id,
                        exit_code = exit_code,
                        "session terminated gracefully"
                    );
                    self.sessions.remove(&session_id);
                    return Ok(exit_code);
                }
                Ok(Err(e)) => {
                    warn!(
                        session_id = %session_id,
                        error = %e,
                        "error waiting for child"
                    );
                }
                Err(_) => {
                    warn!(
                        session_id = %session_id,
                        "graceful shutdown timed out, killing process"
                    );
                }
            }

            if let Err(e) = child.kill().await {
                warn!(session_id = %session_id, error = %e, "failed to kill child process");
            }

            let exit_code = child.wait().await?.code().unwrap_or(-1);
            self.sessions.remove(&session_id);
            return Ok(exit_code);
        }

        self.sessions.remove(&session_id);
        Ok(0)
    }

    async fn write_mcp_config(&self, workspace_path: &str) -> Result<()> {
        let mcp_config = serde_json::json!({
            "mcpServers": {
                "rustbrain": {
                    "command": "npx",
                    "args": ["-y", "@rustbrain/mcp-server"],
                    "env": {
                        "RUSTBRAIN_API_URL": std::env::var("CONTROL_API_URL")
                            .unwrap_or_else(|_| "http://localhost:8080".to_owned())
                    }
                }
            }
        });

        let config_path = format!("{workspace_path}/.mcp.json");
        tokio::fs::write(&config_path, serde_json::to_string_pretty(&mcp_config)?)
            .await
            .context("failed to write MCP config")?;

        Ok(())
    }

    async fn write_opencode_config(&self, workspace_path: &str, cmd: &AgentCommand) -> Result<()> {
        let anthropic_key = std::env::var("ANTHROPIC_API_KEY")
            .unwrap_or_default();

        let config = serde_json::json!({
            "provider": "anthropic",
            "model": cmd.model,
            "api_key": anthropic_key,
        });

        let config_dir = format!("{workspace_path}/.opencode");
        tokio::fs::create_dir_all(&config_dir)
            .await
            .context("failed to create .opencode directory")?;

        let config_path = format!("{config_dir}/config.json");
        tokio::fs::write(&config_path, serde_json::to_string_pretty(&config)?)
            .await
            .context("failed to write opencode config")?;

        Ok(())
    }
}

fn revoke_session_api_key(_session_id: Uuid, _tenant_id: Uuid) {
    info!("revoking session API key");
}

fn generate_session_api_key(session_id: Uuid, tenant_id: Uuid) -> String {
    let key_bytes: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
    let api_key = format!("rb_{}_{}", session_id.simple(), hex::encode(&key_bytes[..16]));

    info!(
        session_id = %session_id,
        tenant_id = %tenant_id,
        "generated session API key"
    );

    api_key
}

fn build_runtime_command(
    runtime_kind: &str,
    cmd: &AgentCommand,
) -> Result<(String, Vec<String>)> {
    match runtime_kind {
        "claude_code" => {
            let mut args = vec![
                "--model".to_owned(),
                cmd.model.clone(),
                "--system-prompt".to_owned(),
                cmd.system_prompt.clone(),
            ];
            if !cmd.initial_message.is_empty() {
                args.push("--message".to_owned());
                args.push(cmd.initial_message.clone());
            }
            Ok(("claude".to_owned(), args))
        }
        "opencode" => {
            let mut args = vec![
                "--model".to_owned(),
                cmd.model.clone(),
            ];
            if !cmd.system_prompt.is_empty() {
                args.push("--system".to_owned());
                args.push(cmd.system_prompt.clone());
            }
            if !cmd.initial_message.is_empty() {
                args.push(cmd.initial_message.clone());
            }
            Ok(("opencode".to_owned(), args))
        }
        "pi" => {
            anyhow::bail!("Pi runtime is not yet implemented (stub)")
        }
        _ => anyhow::bail!("unknown runtime kind: {runtime_kind}"),
    }
}
