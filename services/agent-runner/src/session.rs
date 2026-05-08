use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use rb_schemas::{
    AgentEvent, AgentEventKind, AgentRuntime, AgentSessionInput, AgentSessionStart,
    AgentSessionTerminate, TenantId,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{ChildStderr, ChildStdout};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::Instrument;

use crate::adapters::{
    AgentProcess, RuntimeAdapter, SessionCtx, adapter_for_runtime,
};

pub struct SessionManager {
    sessions: Arc<Mutex<HashMap<String, SessionHandle>>>,
    workspace_base: PathBuf,
    seq_counters: Arc<Mutex<HashMap<String, i64>>>,
    control_api_base: String,
    http_client: reqwest::Client,
}

struct SessionHandle {
    process: AgentProcess,
    start_time: Instant,
    stdout_handle: JoinHandle<()>,
    stderr_handle: JoinHandle<()>,
    tenant_id: TenantId,
}

impl SessionManager {
    pub fn new(workspace_base: PathBuf, control_api_base: String, http_client: reqwest::Client) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            workspace_base,
            seq_counters: Arc::new(Mutex::new(HashMap::new())),
            control_api_base,
            http_client,
        }
    }

    pub async fn start_session(
        &self,
        cmd: &AgentSessionStart,
        tenant_id: TenantId,
        session_id: &str,
        event_sender: tokio::sync::mpsc::Sender<(TenantId, AgentEvent)>,
    ) -> Result<()> {
        let workspace_path = self.workspace_base.join(&cmd.workspace_path);

        std::fs::create_dir_all(&workspace_path)
            .with_context(|| format!("Failed to create workspace: {}", workspace_path.display()))?;

        // Enforce mode 0700 for tenant isolation
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&workspace_path, std::fs::Permissions::from_mode(0o700))
                .with_context(|| {
                    format!("Failed to set 0700 on {}", workspace_path.display())
                })?;
        }

        let ctx = SessionCtx {
            session_id: session_id.to_string(),
            tenant_id: tenant_id.to_string(),
            workspace_path: workspace_path.clone(),
            api_key: cmd.api_key.clone(),
            initial_prompt: cmd.initial_prompt.clone(),
        };

        let runtime = AgentRuntime::try_from(cmd.runtime)
            .map_err(|_| anyhow::anyhow!("Invalid runtime value: {}", cmd.runtime))?;

        let adapter = adapter_for_runtime(runtime);
        let mut process = adapter
            .spawn(&ctx)
            .await
            .with_context(|| format!("Failed to spawn {runtime:?} adapter"))?;

        let pid = process.pid;

        // Report running status to control-api
        self.update_session_status(session_id, "running", Some(i64::from(pid)), None).await;

        let stdout = process.child.stdout.take().context("Process stdout not available")?;
        let stderr = process.child.stderr.take().context("Process stderr not available")?;

        let (stdout_handle, stderr_handle) = self.spawn_output_handlers(
            session_id.to_string(),
            tenant_id,
            stdout,
            stderr,
            event_sender.clone(),
            adapter,
        );

        {
            let mut sessions = self.sessions.lock().await;
            sessions.insert(
                session_id.to_string(),
                SessionHandle {
                    process,
                    start_time: Instant::now(),
                    stdout_handle,
                    stderr_handle,
                    tenant_id,
                },
            );
        }

        self.emit_lifecycle_event(
            tenant_id,
            session_id,
            0,
            AgentEventKind::Started,
            "{}",
            &event_sender,
        )
        .await;

        tracing::info!(session_id = %session_id, pid = pid, runtime = ?runtime, "Session started");
        Ok(())
    }

    pub async fn send_input(&self, session_id: &str, input: &AgentSessionInput) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        let handle = sessions.get_mut(session_id).context("Session not found")?;
        let adapter = adapter_for_runtime(handle.process.runtime);
        adapter.send_input(&mut handle.process, &input.input).await
    }

    pub async fn terminate_session(
        &self,
        session_id: &str,
        terminate: &AgentSessionTerminate,
        event_sender: tokio::sync::mpsc::Sender<(TenantId, AgentEvent)>,
    ) -> Result<()> {
        let mut handle = {
            let mut sessions = self.sessions.lock().await;
            sessions.remove(session_id).context("Session not found")?
        };

        let adapter = adapter_for_runtime(handle.process.runtime);
        let _ = adapter.terminate(&mut handle.process, terminate.force).await;

        // Wait for process exit and capture exit code
        let exit_code = match handle.process.child.wait().await {
            Ok(status) => status.code().unwrap_or(-1),
            Err(_) => -1,
        };

        let duration_ms =
            i64::try_from(handle.start_time.elapsed().as_millis()).unwrap_or(i64::MAX);

        handle.stdout_handle.abort();
        handle.stderr_handle.abort();

        // Report terminated status to control-api
        self.update_session_status(session_id, "terminated", None, Some(exit_code))
            .await;

        // Revoke session-scoped API key
        self.revoke_api_key(session_id).await;

        self.emit_terminated_event(
            handle.tenant_id,
            session_id,
            exit_code,
            duration_ms,
            &terminate.reason,
            event_sender,
        )
        .await;

        tracing::info!(
            session_id = %session_id,
            exit_code = exit_code,
            duration_ms = duration_ms,
            "Session terminated"
        );
        Ok(())
    }

    async fn update_session_status(
        &self,
        session_id: &str,
        status: &str,
        pid: Option<i64>,
        exit_code: Option<i32>,
    ) {
        let url = format!(
            "{}/internal/agent/sessions/{}/status",
            self.control_api_base, session_id
        );
        let body = serde_json::json!({ "status": status, "pid": pid, "exit_code": exit_code });
        if let Err(e) = self.http_client.patch(&url).json(&body).send().await {
            tracing::warn!(session_id = %session_id, "Failed to update session status: {e}");
        }
    }

    async fn revoke_api_key(&self, session_id: &str) {
        let url = format!(
            "{}/internal/agent/sessions/{}/api-key",
            self.control_api_base, session_id
        );
        if let Err(e) = self.http_client.delete(&url).send().await {
            tracing::warn!(session_id = %session_id, "Failed to revoke API key: {e}");
        }
    }

    fn spawn_output_handlers(
        &self,
        session_id: String,
        tenant_id: TenantId,
        stdout: ChildStdout,
        stderr: ChildStderr,
        event_sender: tokio::sync::mpsc::Sender<(TenantId, AgentEvent)>,
        adapter: Box<dyn RuntimeAdapter>,
    ) -> (JoinHandle<()>, JoinHandle<()>) {
        let seq_counters = self.seq_counters.clone();
        let sid_stdout = session_id.clone();
        let span_out = tracing::info_span!("stdout_handler", session_id = %sid_stdout);

        let stdout_handle = tokio::spawn(
            {
                let es = event_sender.clone();
                let adapter = adapter;
                async move {
                    let reader = BufReader::new(stdout);
                    let mut lines = reader.lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let seq = {
                            let mut c = seq_counters.lock().await;
                            let n = c.entry(sid_stdout.clone()).or_insert(0);
                            *n += 1;
                            *n
                        };
                        if let Some(parsed) = adapter.parse_stdout_line(&line) {
                            let event = AgentEvent {
                                tenant_id: tenant_id.to_string(),
                                session_id: sid_stdout.clone(),
                                seq,
                                kind: AgentEventKind::Stdout.into(),
                                payload: parsed.payload,
                                emitted_at_ms: chrono::Utc::now().timestamp_millis(),
                            };
                            let _ = es.send((tenant_id, event)).await;
                        }
                    }
                }
            }
            .instrument(span_out),
        );

        let seq_counters2 = self.seq_counters.clone();
        let sid_err = session_id;
        let span_err = tracing::info_span!("stderr_handler", session_id = %sid_err);

        let stderr_handle = tokio::spawn(
            async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let seq = {
                        let mut c = seq_counters2.lock().await;
                        let n = c.entry(sid_err.clone()).or_insert(0);
                        *n += 1;
                        *n
                    };
                    let event = AgentEvent {
                        tenant_id: tenant_id.to_string(),
                        session_id: sid_err.clone(),
                        seq,
                        kind: AgentEventKind::Stderr.into(),
                        payload: line,
                        emitted_at_ms: chrono::Utc::now().timestamp_millis(),
                    };
                    let _ = event_sender.send((tenant_id, event)).await;
                }
            }
            .instrument(span_err),
        );

        (stdout_handle, stderr_handle)
    }

    async fn emit_lifecycle_event(
        &self,
        tenant_id: TenantId,
        session_id: &str,
        seq: i64,
        kind: AgentEventKind,
        payload: &str,
        event_sender: &tokio::sync::mpsc::Sender<(TenantId, AgentEvent)>,
    ) {
        let event = AgentEvent {
            tenant_id: tenant_id.to_string(),
            session_id: session_id.to_string(),
            seq,
            kind: kind.into(),
            payload: payload.to_string(),
            emitted_at_ms: chrono::Utc::now().timestamp_millis(),
        };
        let _ = event_sender.send((tenant_id, event)).await;
    }

    async fn emit_terminated_event(
        &self,
        tenant_id: TenantId,
        session_id: &str,
        exit_code: i32,
        duration_ms: i64,
        reason: &str,
        event_sender: tokio::sync::mpsc::Sender<(TenantId, AgentEvent)>,
    ) {
        let payload = serde_json::json!({
            "exit_code": exit_code,
            "duration_ms": duration_ms,
            "reason": reason,
        });
        let event = AgentEvent {
            tenant_id: tenant_id.to_string(),
            session_id: session_id.to_string(),
            seq: -1,
            kind: AgentEventKind::Terminated.into(),
            payload: payload.to_string(),
            emitted_at_ms: chrono::Utc::now().timestamp_millis(),
        };
        let _ = event_sender.send((tenant_id, event)).await;
    }
}

pub fn spawn_workspace_gc(workspace_base: PathBuf) {
    let ttl_days = std::env::var("RB_AGENT_WORKSPACE_TTL_DAYS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(7);
    let ttl = std::time::Duration::from_secs(ttl_days * 24 * 3600);

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(6 * 3600));
        loop {
            interval.tick().await;
            gc_workspaces(&workspace_base, ttl);
        }
    });
}

fn gc_workspaces(base: &PathBuf, ttl: std::time::Duration) {
    let now = std::time::SystemTime::now();
    let Ok(tenant_dirs) = std::fs::read_dir(base) else {
        return;
    };

    for tenant_entry in tenant_dirs.flatten() {
        let Ok(session_dirs) = std::fs::read_dir(tenant_entry.path()) else {
            continue;
        };
        for session_entry in session_dirs.flatten() {
            let path = session_entry.path();
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let Ok(mtime) = meta.modified() else {
                continue;
            };
            let Ok(age) = now.duration_since(mtime) else {
                continue;
            };
            if age > ttl {
                if let Err(e) = std::fs::remove_dir_all(&path) {
                    tracing::warn!("GC: failed to remove {}: {e}", path.display());
                } else {
                    tracing::info!("GC: removed expired workspace {}", path.display());
                }
            }
        }
    }
}
