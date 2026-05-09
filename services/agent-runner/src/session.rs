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

/// Maximum time to wait for graceful process termination before SIGKILL.
const PROCESS_TERMINATE_TIMEOUT_SECS: u64 = 30;

/// Maximum allowed length for `initial_prompt` to prevent denial-of-service.
const MAX_INITIAL_PROMPT_LEN: usize = 100_000;

/// Maximum number of tracked sessions to prevent unbounded memory growth.
const MAX_TRACKED_SESSIONS: usize = 100_000;

pub struct SessionManager {
    sessions: Arc<Mutex<HashMap<String, SessionHandle>>>,
    workspace_base: PathBuf,
    seq_counters: Arc<Mutex<HashMap<String, i64>>>,
    control_api_base: String,
    http_client: reqwest::Client,
}

struct SessionHandle {
    // Per-session mutex so send_input never holds the sessions map lock across I/O.
    process: Arc<Mutex<AgentProcess>>,
    start_time: Instant,
    stdout_handle: JoinHandle<()>,
    stderr_handle: JoinHandle<()>,
    tenant_id: TenantId,
}

/// Validate that `rel` is a safe relative path (no `..`, no `.`, not absolute).
/// Returns the joined absolute path on success.
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

/// Interval for cleaning up stale seq counter entries to prevent unbounded growth.
const SEQ_COUNTER_GC_INTERVAL_SECS: u64 = 300; // 5 minutes

impl SessionManager {
    pub fn new(
        workspace_base: PathBuf,
        control_api_base: String,
        http_client: reqwest::Client,
    ) -> Self {
        let seq_counters = Arc::new(Mutex::new(HashMap::new()));

        // H9: Spawn periodic garbage collection for seq_counters to prevent unbounded growth
        let seq_counters_gc = Arc::clone(&seq_counters);
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(SEQ_COUNTER_GC_INTERVAL_SECS));
            loop {
                interval.tick().await;
                let mut counters = seq_counters_gc.lock().await;
                let before = counters.len();
                // Keep only entries that have seen recent activity (>1 seq)
                counters.retain(|_, v| *v > 1);
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
        if cmd.initial_prompt.len() > MAX_INITIAL_PROMPT_LEN {
            anyhow::bail!(
                "initial_prompt exceeds maximum length of {MAX_INITIAL_PROMPT_LEN} bytes"
            );
        }

        // SECURITY: validate workspace_path before joining to prevent path traversal.
        let workspace_path = safe_join(self.workspace_base.as_path(), &cmd.workspace_path)
            .with_context(|| format!("Rejected workspace_path: {:?}", cmd.workspace_path))?;

        tokio::fs::create_dir_all(&workspace_path)
            .await
            .with_context(|| format!("Failed to create workspace: {}", workspace_path.display()))?;

        // Enforce mode 0700 for tenant isolation
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
                return Err(e.context(format!("Failed to spawn {runtime:?} adapter")));
            }
        };

        let pid = process.pid;

        // Report running status to control-api
        self.update_session_status(session_id, tenant_id, "running", Some(i64::from(pid)), None)
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

        {
            let mut sessions = self.sessions.lock().await;
            if sessions.len() >= MAX_TRACKED_SESSIONS {
                anyhow::bail!(
                    "Maximum number of concurrent sessions ({MAX_TRACKED_SESSIONS}) exceeded"
                );
            }
            sessions.insert(
                session_id.to_string(),
                SessionHandle {
                    process: process_arc,
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
        // Acquire only the sessions map lock long enough to clone the per-session Arc.
        // The per-session lock is then held for the I/O, not the whole sessions map.
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
            counters.remove(session_id);
        }

        let exit_code = {
            let mut proc = handle.process.lock().await;
            let adapter = adapter_for_runtime(proc.runtime)?;
            let _ = adapter.terminate(&mut proc, terminate.force).await;

            // H5: Wait for process exit with timeout to prevent unbounded stall
            let timeout_duration = Duration::from_secs(PROCESS_TERMINATE_TIMEOUT_SECS);
            match tokio::time::timeout(timeout_duration, proc.child.wait()).await {
                Ok(Ok(status)) => status.code().unwrap_or(-1),
                Ok(Err(_)) => -1,
                Err(_) => {
                    // Timeout: force kill the process
                    tracing::warn!(session_id = %session_id, "Process termination timeout, forcing SIGKILL");
                    let _ = adapter.terminate(&mut proc, true).await;
                    // Wait again briefly for forced termination
                    match tokio::time::timeout(Duration::from_secs(5), proc.child.wait()).await {
                        Ok(Ok(status)) => status.code().unwrap_or(-1),
                        _ => -1,
                    }
                }
            }
        };

        let duration_ms =
            i64::try_from(handle.start_time.elapsed().as_millis()).unwrap_or(i64::MAX);

        handle.stdout_handle.abort();
        handle.stderr_handle.abort();

        // Report terminated status to control-api
        self.update_session_status(
            session_id,
            handle.tenant_id,
            "terminated",
            None,
            Some(exit_code),
        )
        .await;

        // Revoke session-scoped API key
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
                        // H1: Safely increment seq with overflow protection
                        let seq = {
                            let mut c = seq_counters.lock().await;
                            let n = c.entry(sid_stdout.clone()).or_insert(0);
                            // Check for i64::MAX to prevent overflow
                            if *n >= i64::MAX - 1 {
                                tracing::warn!(session_id = %sid_stdout, "Seq counter approaching overflow, wrapping to 1");
                                *n = 1;
                            } else {
                                *n += 1;
                            }
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
                    // H1: Safely increment seq with overflow protection
                    let seq = {
                        let mut c = seq_counters2.lock().await;
                        let n = c.entry(sid_err.clone()).or_insert(0);
                        // Check for i64::MAX to prevent overflow
                        if *n >= i64::MAX - 1 {
                            tracing::warn!(session_id = %sid_err, "Seq counter approaching overflow, wrapping to 1");
                            *n = 1;
                        } else {
                            *n += 1;
                        }
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
            std::time::Duration::from_secs(5),
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
        // H4: Use distinct seq range to avoid collision with error events
        // Error events use i64::MIN + 1, terminated uses i64::MIN + 2
        const TERMINATED_SEQ: i64 = i64::MIN + 2;

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
            std::time::Duration::from_secs(5),
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
            let base = workspace_base.clone();
            tokio::task::spawn_blocking(move || gc_workspaces(&base, ttl))
                .await
                .ok();
        }
    });
}

fn gc_workspaces(base: &PathBuf, ttl: std::time::Duration) {
    let now = std::time::SystemTime::now();
    let Ok(tenant_dirs) = std::fs::read_dir(base) else {
        return;
    };

    for tenant_entry in tenant_dirs.flatten() {
        // H5: Validate tenant directory name to prevent escaping workspace_base
        let tenant_name = tenant_entry.file_name();
        let tenant_str = tenant_name.to_string_lossy();
        // Basic sanity check: tenant dirs should be valid identifiers
        if tenant_str.contains('/') || tenant_str.contains("..") {
            tracing::warn!("GC: skipping suspicious tenant directory: {}", tenant_str);
            continue;
        }

        let Ok(session_dirs) = std::fs::read_dir(tenant_entry.path()) else {
            continue;
        };
        for session_entry in session_dirs.flatten() {
            let path = session_entry.path();

            // H5: Validate session directory is within expected tenant structure
            let Ok(relative_path) = path.strip_prefix(base) else {
                tracing::warn!(
                    "GC: skipping path outside workspace base: {}",
                    path.display()
                );
                continue;
            };
            let components: Vec<_> = relative_path.components().collect();
            if components.len() != 2 {
                tracing::warn!("GC: skipping unexpected path structure: {}", path.display());
                continue;
            }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_rejects_parent_traversal() {
        let base = PathBuf::from("/data/workspaces");
        assert!(safe_join(&base, "../etc/passwd").is_err());
        assert!(safe_join(&base, "tenant/../../etc").is_err());
        assert!(safe_join(&base, "/absolute/path").is_err());
    }

    #[test]
    fn safe_join_accepts_valid_relative_paths() {
        let base = PathBuf::from("/data/workspaces");
        let result = safe_join(&base, "tenant-abc/session-xyz");
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            PathBuf::from("/data/workspaces/tenant-abc/session-xyz")
        );
    }

    #[test]
    fn safe_join_accepts_simple_name() {
        let base = PathBuf::from("/data/workspaces");
        let result = safe_join(&base, "mysession");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), PathBuf::from("/data/workspaces/mysession"));
    }
}
