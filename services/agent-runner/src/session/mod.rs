use std::collections::HashMap;
use std::path::{Component, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use metrics::counter;
use rb_schemas::{
    AgentEvent, AgentEventKind, AgentRuntime, AgentSessionInput, AgentSessionStart,
    AgentSessionTerminate, TenantId,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{ChildStderr, ChildStdout};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::Instrument;

use crate::adapters::{AgentProcess, RuntimeAdapter, SessionCtx, adapter_for_runtime};

mod natural_exit;

const PROCESS_TERMINATE_TIMEOUT_SECS: u64 = 30;
const MAX_INITIAL_PROMPT_LEN: usize = 100_000;
const MAX_TRACKED_SESSIONS: usize = 100_000;

const SEQ_COUNTER_GC_INTERVAL_SECS: u64 = 300;
const SEQ_COUNTER_MAX_AGE_SECS: u64 = 600;

/// Sentinel seq value for Terminated / natural-exit lifecycle events.
/// Must match `lifecycle_event_seq("terminated")` in control-api/src/routes/agents/sessions.rs.
const TERMINATED_SEQ: i64 = i64::MIN + 2;

pub struct SessionManager {
    sessions: Arc<Mutex<HashMap<String, SessionHandle>>>,
    workspace_base: PathBuf,
    seq_counters: Arc<Mutex<HashMap<String, i64>>>,
    seq_counter_timestamps: Arc<Mutex<HashMap<String, Instant>>>,
    control_api_base: String,
    http_client: reqwest::Client,
    relay_sender: agent_runner::EventSender,
    /// SHA of the mcp-server-node bundle baked into the agent-runner image.
    mcp_sha: String,
}

struct SessionHandle {
    process: Arc<Mutex<AgentProcess>>,
    start_time: Instant,
    stdout_handle: JoinHandle<()>,
    stderr_handle: JoinHandle<()>,
    /// Watches for natural (unforced) process exit and transitions the session
    /// to `terminated` or `failed` automatically.  Aborted by
    /// `terminate_session` when an explicit termination wins the race.
    wait_handle: JoinHandle<()>,
    tenant_id: TenantId,
}

fn safe_join(base: &std::path::Path, rel: &str) -> Result<PathBuf> {
    let rel_path = std::path::Path::new(rel);
    if rel_path.is_absolute() {
        anyhow::bail!("workspace_path must be relative, got absolute path");
    }
    for component in rel_path.components() {
        match component {
            Component::ParentDir | Component::CurDir => {
                anyhow::bail!("workspace_path contains disallowed path components");
            }
            _ => {}
        }
    }
    Ok(base.join(rel_path))
}

impl SessionManager {
    pub fn new(
        workspace_base: PathBuf,
        control_api_base: String,
        http_client: reqwest::Client,
        relay_sender: agent_runner::EventSender,
        mcp_sha: String,
    ) -> Self {
        let seq_counters = Arc::new(Mutex::new(HashMap::new()));
        let seq_counter_timestamps: Arc<Mutex<HashMap<String, Instant>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let seq_counters_gc = Arc::clone(&seq_counters);
        let timestamps_gc = Arc::clone(&seq_counter_timestamps);
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(Duration::from_secs(SEQ_COUNTER_GC_INTERVAL_SECS));
            let max_age = Duration::from_secs(SEQ_COUNTER_MAX_AGE_SECS);
            loop {
                interval.tick().await;
                let now = Instant::now();
                let mut counters = seq_counters_gc.lock().await;
                let mut timestamps = timestamps_gc.lock().await;
                let before = counters.len();
                timestamps.retain(|session_id, ts| {
                    let retain = now.duration_since(*ts) < max_age;
                    if !retain {
                        counters.remove(session_id);
                    }
                    retain
                });
                let removed = before - counters.len();
                if removed > 0 {
                    tracing::debug!("GC: removed {} stale seq counter entries", removed);
                }
            }
        });

        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            workspace_base,
            seq_counters,
            seq_counter_timestamps,
            control_api_base,
            http_client,
            relay_sender,
            mcp_sha,
        }
    }

    #[allow(clippy::too_many_lines)]
    pub async fn start_session(
        &self,
        cmd: &AgentSessionStart,
        tenant_id: TenantId,
        session_id: &str,
        event_sender: tokio::sync::mpsc::Sender<(TenantId, AgentEvent)>,
    ) -> Result<()> {
        if cmd.initial_prompt.len() > MAX_INITIAL_PROMPT_LEN {
            anyhow::bail!(
                "initial_prompt exceeds maximum length of {MAX_INITIAL_PROMPT_LEN} bytes"
            );
        }

        let workspace_path = safe_join(self.workspace_base.as_path(), &cmd.workspace_path)
            .with_context(|| format!("Rejected workspace_path: {:?}", cmd.workspace_path))?;

        tokio::fs::create_dir_all(&workspace_path)
            .await
            .with_context(|| format!("Failed to create workspace: {}", workspace_path.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o700);
            tokio::fs::set_permissions(&workspace_path, perms)
                .await
                .with_context(|| format!("Failed to set 0700 on {}", workspace_path.display()))?;
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

        let adapter = adapter_for_runtime(runtime)?;
        let mut process = match adapter.spawn(&ctx).await {
            Ok(p) => p,
            Err(e) => {
                let _ = tokio::fs::remove_dir_all(&workspace_path).await;
                // Mark the session row `failed` before propagating so it does not
                // accumulate as `pending` forever (per ADR-009 §5 / RUSAA-1179).
                // control-api's PATCH path applies the failed_at/failure_reason
                // columns and decrements TenantSessionCount because `failed` is
                // a terminal status.  Any HTTP error here is logged and dropped;
                // we still want to surface the original spawn error.
                let err_msg = format!("{e:#}");
                self.update_session_status(
                    session_id,
                    tenant_id,
                    "failed",
                    None,
                    None,
                    Some(&err_msg),
                )
                .await;
                return Err(e.context(format!("Failed to spawn {runtime:?} adapter")));
            }
        };

        let pid = process.pid;

        self.update_session_status(
            session_id,
            tenant_id,
            "running",
            Some(i64::from(pid)),
            None,
            None,
        )
        .await;

        let stdout = process
            .child
            .stdout
            .take()
            .context("Process stdout not available")?;
        let stderr = process
            .child
            .stderr
            .take()
            .context("Process stderr not available")?;

        let (stdout_handle, stderr_handle) = self.spawn_output_handlers(
            session_id.to_string(),
            tenant_id,
            stdout,
            stderr,
            event_sender.clone(),
            adapter,
        );

        let process_arc = Arc::new(Mutex::new(process));
        let start_time = Instant::now();

        let wait_handle = natural_exit::spawn_natural_exit_handler(
            session_id.to_string(),
            tenant_id,
            Arc::clone(&process_arc),
            Arc::clone(&self.sessions),
            self.seq_counters.clone(),
            self.seq_counter_timestamps.clone(),
            self.control_api_base.clone(),
            self.http_client.clone(),
            event_sender.clone(),
        );

        {
            let mut sessions = self.sessions.lock().await;
            if sessions.len() >= MAX_TRACKED_SESSIONS {
                wait_handle.abort();
                anyhow::bail!(
                    "Maximum number of concurrent sessions ({MAX_TRACKED_SESSIONS}) exceeded"
                );
            }
            sessions.insert(
                session_id.to_string(),
                SessionHandle {
                    process: process_arc,
                    start_time,
                    stdout_handle,
                    stderr_handle,
                    wait_handle,
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

        // Cross-runtime SHA pairing (warn-only per board decision in RUSAA-1644).
        // Logs runner and MCP bundle SHAs so dashboards can detect cross-commit
        // divergence without blocking session spawn.
        let runner_sha = rb_build_info::SHA;
        let mcp_sha = self.mcp_sha.as_str();
        let mcp_sha_mismatch =
            runner_sha != "unknown" && mcp_sha != "unknown" && runner_sha != mcp_sha;
        tracing::info!(
            session_id = %session_id,
            runner_sha,
            mcp_sha,
            mcp_sha_mismatch,
            "MCP session SHA pair"
        );
        tracing::info!(session_id = %session_id, pid = pid, runtime = ?runtime, "Session started");
        Ok(())
    }

    pub async fn send_input(&self, session_id: &str, input: &AgentSessionInput) -> Result<()> {
        let process = {
            let sessions = self.sessions.lock().await;
            sessions
                .get(session_id)
                .map(|h| Arc::clone(&h.process))
                .context("Session not found")?
        };
        let mut proc = process.lock().await;
        let adapter = adapter_for_runtime(proc.runtime)?;
        adapter.send_input(&mut proc, &input.input).await
    }

    pub async fn terminate_all(&self) {
        let session_ids: Vec<String> = {
            let sessions = self.sessions.lock().await;
            sessions.keys().cloned().collect()
        };

        for session_id in session_ids {
            let process = {
                let sessions = self.sessions.lock().await;
                sessions.get(&session_id).map(|h| Arc::clone(&h.process))
            };

            if let Some(proc_arc) = process {
                let mut proc = proc_arc.lock().await;
                if let Ok(adapter) = adapter_for_runtime(proc.runtime) {
                    let _ = adapter.terminate(&mut proc, false).await;
                    let timeout = Duration::from_secs(PROCESS_TERMINATE_TIMEOUT_SECS);
                    let _ = tokio::time::timeout(timeout, proc.child.wait()).await;
                }
            }
        }
    }

    pub async fn terminate_session(
        &self,
        session_id: &str,
        terminate: &AgentSessionTerminate,
        event_sender: tokio::sync::mpsc::Sender<(TenantId, AgentEvent)>,
    ) -> Result<()> {
        let handle = {
            let mut sessions = self.sessions.lock().await;
            sessions.remove(session_id).context("Session not found")?
        };

        {
            let mut counters = self.seq_counters.lock().await;
            let mut timestamps = self.seq_counter_timestamps.lock().await;
            counters.remove(session_id);
            timestamps.remove(session_id);
        }

        // Abort I/O and wait handlers before taking the process lock so the
        // natural-exit handler cannot win the cleanup race after we removed
        // the session from the map.
        handle.stdout_handle.abort();
        handle.stderr_handle.abort();
        handle.wait_handle.abort();

        let exit_code = {
            let mut proc = handle.process.lock().await;
            let adapter = adapter_for_runtime(proc.runtime)?;
            let _ = adapter.terminate(&mut proc, terminate.force).await;

            let timeout_duration = Duration::from_secs(PROCESS_TERMINATE_TIMEOUT_SECS);
            match tokio::time::timeout(timeout_duration, proc.child.wait()).await {
                Ok(Ok(status)) => status.code().unwrap_or(-1),
                Ok(Err(_)) => -1,
                Err(_) => {
                    tracing::warn!(session_id = %session_id, "Process termination timeout, forcing SIGKILL");
                    let _ = adapter.terminate(&mut proc, true).await;
                    match tokio::time::timeout(Duration::from_secs(5), proc.child.wait()).await {
                        Ok(Ok(status)) => status.code().unwrap_or(-1),
                        _ => -1,
                    }
                }
            }
        };

        let duration_ms =
            i64::try_from(handle.start_time.elapsed().as_millis()).unwrap_or(i64::MAX);

        self.update_session_status(
            session_id,
            handle.tenant_id,
            "terminated",
            None,
            Some(exit_code),
            None,
        )
        .await;

        self.revoke_api_key(session_id).await;

        if exit_code != 0 {
            tracing::error!(
                session_id = %session_id,
                exit_code = exit_code,
                duration_ms = duration_ms,
                reason = %terminate.reason,
                "Agent session terminated with non-zero exit code"
            );
        }

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
        tenant_id: TenantId,
        status: &str,
        pid: Option<i64>,
        exit_code: Option<i32>,
        error: Option<&str>,
    ) {
        let Ok(validated_id) = uuid::Uuid::parse_str(session_id) else {
            tracing::warn!(session_id = %session_id, "Rejected non-UUID session_id in status update");
            return;
        };
        let url = format!(
            "{}/internal/agent/sessions/{}/status",
            self.control_api_base, validated_id
        );
        let body = serde_json::json!({
            "status": status,
            "pid": pid,
            "exit_code": exit_code,
            "error": error,
            "tenant_id": tenant_id.to_string(),
        });
        if let Err(e) = self.http_client.patch(&url).json(&body).send().await {
            tracing::warn!(session_id = %session_id, "Failed to update session status: {e}");
        }
    }

    async fn revoke_api_key(&self, session_id: &str) {
        let Ok(validated_id) = uuid::Uuid::parse_str(session_id) else {
            tracing::warn!(session_id = %session_id, "Rejected non-UUID session_id in key revocation");
            return;
        };
        let url = format!(
            "{}/internal/agent/sessions/{}/api-key",
            self.control_api_base, validated_id
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
        let seq_timestamps = self.seq_counter_timestamps.clone();
        let relay_sender = self.relay_sender.clone();
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
                            let mut ts = seq_timestamps.lock().await;
                            let n = c.entry(sid_stdout.clone()).or_insert(0);
                            if *n >= i64::MAX - 1 {
                                tracing::warn!(session_id = %sid_stdout, "Seq counter approaching overflow, wrapping to 1");
                                *n = 1;
                            } else {
                                *n += 1;
                            }
                            ts.insert(sid_stdout.clone(), Instant::now());
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
                            if let Err(e) = es.try_send((tenant_id, event)) {
                                tracing::error!(session_id = %sid_stdout, error = %e, "Failed to send stdout event (channel full or closed)");
                            }
                        }
                        agent_runner::relay_stdout_events(&relay_sender, &sid_stdout, &tenant_id.to_string(), seq, &line);
                    }
                }
            }
            .instrument(span_out),
        );

        let seq_counters2 = self.seq_counters.clone();
        let seq_timestamps2 = self.seq_counter_timestamps.clone();
        let sid_err = session_id;
        let span_err = tracing::info_span!("stderr_handler", session_id = %sid_err);

        let stderr_handle = tokio::spawn(
            async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let seq = {
                        let mut c = seq_counters2.lock().await;
                        let mut ts = seq_timestamps2.lock().await;
                        let n = c.entry(sid_err.clone()).or_insert(0);
                        if *n >= i64::MAX - 1 {
                            tracing::warn!(session_id = %sid_err, "Seq counter approaching overflow, wrapping to 1");
                            *n = 1;
                        } else {
                            *n += 1;
                        }
                        ts.insert(sid_err.clone(), Instant::now());
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
                    if let Err(e) = event_sender.try_send((tenant_id, event)) {
                        tracing::error!(session_id = %sid_err, error = %e, "Failed to send stderr event (channel full or closed)");
                    }
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
        if tokio::time::timeout(
            Duration::from_secs(5),
            event_sender.send((tenant_id, event)),
        )
        .await
        .is_err()
        {
            tracing::warn!(session_id = %session_id, seq = seq, "Event channel full, dropped lifecycle event");
            counter!("rb_agent_events_dropped_total", "reason" => "channel_full").increment(1);
        }
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
            seq: TERMINATED_SEQ,
            kind: AgentEventKind::Terminated.into(),
            payload: payload.to_string(),
            emitted_at_ms: chrono::Utc::now().timestamp_millis(),
        };
        if tokio::time::timeout(
            Duration::from_secs(5),
            event_sender.send((tenant_id, event)),
        )
        .await
        .is_err()
        {
            tracing::warn!(session_id = %session_id, "Event channel full, dropped terminated event");
            counter!("rb_agent_events_dropped_total", "reason" => "channel_full").increment(1);
        }
    }
}
pub use crate::workspace_gc::spawn_workspace_gc;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
