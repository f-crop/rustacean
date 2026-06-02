use super::natural_exit::spawn_natural_exit_handler;
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

// ---------------------------------------------------------------------
// RUSAA-1179: spawn-failure marks session `failed`
// ---------------------------------------------------------------------

/// Minimal in-process HTTP server that captures PATCH requests so the test
/// can assert agent-runner's status callback without pulling in wiremock.
async fn spawn_status_capture_server() -> (
    std::net::SocketAddr,
    std::sync::Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
    tokio::task::JoinHandle<()>,
) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let captured: std::sync::Arc<tokio::sync::Mutex<Vec<(String, String)>>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let captured_clone = std::sync::Arc::clone(&captured);

    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let captured = std::sync::Arc::clone(&captured_clone);
            tokio::spawn(async move {
                let (read_half, mut write_half) = sock.split();
                let mut reader = BufReader::new(read_half);
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).await.is_err() {
                    return;
                }
                let mut content_length: usize = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.is_err() {
                        return;
                    }
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    let lower = line.to_ascii_lowercase();
                    if let Some(rest) = lower.strip_prefix("content-length:") {
                        content_length = rest.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; content_length];
                if content_length > 0 && reader.read_exact(&mut body).await.is_err() {
                    return;
                }
                let body_str = String::from_utf8_lossy(&body).to_string();
                {
                    let mut g = captured.lock().await;
                    g.push((request_line.trim().to_owned(), body_str));
                }
                let _ = write_half
                    .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                    .await;
            });
        }
    });

    (addr, captured, handle)
}

#[tokio::test(flavor = "current_thread")]
async fn start_session_marks_failed_on_adapter_spawn_error() {
    use rb_schemas::{AgentRuntime, AgentSessionStart};

    let (addr, captured, server_handle) = spawn_status_capture_server().await;
    let tmp = tempfile::tempdir().unwrap();

    let relay_sender = agent_runner::spawn(agent_runner::RelayConfig {
        capacity: agent_runner::DEFAULT_CAPACITY,
        batch_size: agent_runner::DEFAULT_BATCH_SIZE,
        flush_interval: Duration::from_millis(agent_runner::DEFAULT_FLUSH_INTERVAL_MS),
        control_api_base: format!("http://{addr}"),
        http_client: reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap(),
    });
    let manager = SessionManager::new(
        tmp.path().to_path_buf(),
        format!("http://{addr}"),
        reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap(),
        relay_sender,
        "test-mcp-sha".to_string(),
    );

    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let session_uuid = uuid::Uuid::new_v4();
    let session_id = session_uuid.to_string();
    let tenant_id = TenantId::from(uuid::Uuid::new_v4());

    // Pi adapter unconditionally bails on spawn (ADR-009 Phase 3 pending),
    // which is exactly the faulty-adapter shape we need.
    let start = AgentSessionStart {
        runtime: AgentRuntime::Pi as i32,
        workspace_path: format!("rusaa1179-{session_uuid}"),
        api_key: "test-key".to_owned(),
        initial_prompt: "hello".to_owned(),
    };

    let result = manager
        .start_session(&start, tenant_id, &session_id, tx)
        .await;
    assert!(result.is_err(), "Pi adapter should fail to spawn");

    // Wait up to ~2s for the failed-status PATCH to land at the mock.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let expected_path = format!("/internal/agent/sessions/{session_uuid}/status");
    loop {
        {
            let g = captured.lock().await;
            if g.iter().any(|(rl, body)| {
                rl.starts_with("PATCH ")
                    && rl.contains(&expected_path)
                    && body.contains("\"status\":\"failed\"")
            }) {
                break;
            }
        }
        if tokio::time::Instant::now() > deadline {
            let g = captured.lock().await;
            panic!(
                "expected PATCH {expected_path} with status=failed; captured: {:?}",
                *g
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let g = captured.lock().await;
    let failed = g
        .iter()
        .find(|(rl, body)| {
            rl.starts_with("PATCH ")
                && rl.contains(&expected_path)
                && body.contains("\"status\":\"failed\"")
        })
        .expect("failed-status PATCH must be present");

    // Error string must be propagated as the failure_reason payload.
    assert!(
        failed.1.contains("\"error\":")
            && (failed.1.contains("PiAdapter") || failed.1.contains("pi runtime")),
        "expected error payload describing the Pi spawn failure; got {}",
        failed.1
    );

    // No 'running' status should have been emitted on this code path.
    assert!(
        !g.iter()
            .any(|(_, body)| body.contains("\"status\":\"running\"")),
        "running status must not be reported when spawn fails; captured: {:?}",
        *g
    );

    server_handle.abort();
}

// ---------------------------------------------------------------------
// RUSAA-1267: natural-exit handler transitions session to terminal status
// ---------------------------------------------------------------------

/// Build a minimal `SessionHandle` with stub I/O handles and a real process.
fn make_stub_handle(
    process: Arc<Mutex<crate::adapters::AgentProcess>>,
    tenant_id: TenantId,
) -> SessionHandle {
    // One-permit semaphore so the test handle holds a valid OwnedSemaphorePermit.
    let sem = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = sem
        .try_acquire_owned()
        .expect("fresh semaphore must yield a permit");
    SessionHandle {
        process,
        start_time: Instant::now(),
        stdout_handle: tokio::spawn(async {}),
        stderr_handle: tokio::spawn(async {}),
        wait_handle: tokio::spawn(async {}), // replaced after insertion in real code
        tenant_id,
        _node_permit: permit,
        _tenant_guard: caps::TenantCountGuard::new_defused_for_test(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn natural_exit_zero_sends_terminated_status() {
    use std::process::Stdio;

    let (addr, captured, server_handle) = spawn_status_capture_server().await;

    let mut cmd = tokio::process::Command::new("/bin/true");
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let child = cmd.spawn().unwrap();
    let pid = child.id().unwrap();
    let process = crate::adapters::AgentProcess {
        child,
        pid,
        runtime: rb_schemas::AgentRuntime::ClaudeCode,
    };

    let sessions: Arc<Mutex<HashMap<String, SessionHandle>>> = Arc::new(Mutex::new(HashMap::new()));
    let seq_counters = Arc::new(Mutex::new(HashMap::new()));
    let seq_timestamps = Arc::new(Mutex::new(HashMap::<String, Instant>::new()));
    let process_arc = Arc::new(Mutex::new(process));
    let session_uuid = uuid::Uuid::new_v4();
    let session_id = session_uuid.to_string();
    let tenant_id = TenantId::from(uuid::Uuid::new_v4());

    // Pre-insert a stub handle so the natural-exit handler can remove it.
    {
        let mut map = sessions.lock().await;
        map.insert(
            session_id.clone(),
            make_stub_handle(Arc::clone(&process_arc), tenant_id),
        );
    }

    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let tenant_counts = Arc::new(std::sync::Mutex::new(HashMap::<TenantId, usize>::new()));
    let wait_handle = spawn_natural_exit_handler(
        session_id.clone(),
        tenant_id,
        Arc::clone(&process_arc),
        Arc::clone(&sessions),
        seq_counters,
        seq_timestamps,
        format!("http://{addr}"),
        reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap(),
        tx,
        Arc::clone(&tenant_counts),
    );

    wait_handle.await.expect("natural exit handler panicked");

    let expected_path = format!("/internal/agent/sessions/{session_uuid}/status");
    let g = captured.lock().await;
    let found = g.iter().any(|(rl, body)| {
        rl.starts_with("PATCH ")
            && rl.contains(&expected_path)
            && body.contains("\"status\":\"terminated\"")
    });
    assert!(
        found,
        "expected PATCH {expected_path} with status=terminated; captured: {:?}",
        *g
    );
    // Session should have been removed from the map.
    assert!(
        sessions.lock().await.is_empty(),
        "session must be removed after natural exit"
    );

    server_handle.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn natural_exit_nonzero_sends_failed_status() {
    use std::process::Stdio;

    let (addr, captured, server_handle) = spawn_status_capture_server().await;

    let mut cmd = tokio::process::Command::new("/bin/sh");
    cmd.args(["-c", "exit 42"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let child = cmd.spawn().unwrap();
    let pid = child.id().unwrap();
    let process = crate::adapters::AgentProcess {
        child,
        pid,
        runtime: rb_schemas::AgentRuntime::ClaudeCode,
    };

    let sessions: Arc<Mutex<HashMap<String, SessionHandle>>> = Arc::new(Mutex::new(HashMap::new()));
    let seq_counters = Arc::new(Mutex::new(HashMap::new()));
    let seq_timestamps = Arc::new(Mutex::new(HashMap::<String, Instant>::new()));
    let process_arc = Arc::new(Mutex::new(process));
    let session_uuid = uuid::Uuid::new_v4();
    let session_id = session_uuid.to_string();
    let tenant_id = TenantId::from(uuid::Uuid::new_v4());

    {
        let mut map = sessions.lock().await;
        map.insert(
            session_id.clone(),
            make_stub_handle(Arc::clone(&process_arc), tenant_id),
        );
    }

    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let tenant_counts = Arc::new(std::sync::Mutex::new(HashMap::<TenantId, usize>::new()));
    let wait_handle = spawn_natural_exit_handler(
        session_id.clone(),
        tenant_id,
        Arc::clone(&process_arc),
        Arc::clone(&sessions),
        seq_counters,
        seq_timestamps,
        format!("http://{addr}"),
        reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap(),
        tx,
        Arc::clone(&tenant_counts),
    );

    wait_handle.await.expect("natural exit handler panicked");

    let expected_path = format!("/internal/agent/sessions/{session_uuid}/status");
    let g = captured.lock().await;
    let found = g.iter().any(|(rl, body)| {
        rl.starts_with("PATCH ")
            && rl.contains(&expected_path)
            && body.contains("\"status\":\"failed\"")
    });
    assert!(
        found,
        "expected PATCH {expected_path} with status=failed; captured: {:?}",
        *g
    );

    server_handle.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn natural_exit_noop_when_session_already_removed() {
    use std::process::Stdio;

    let (addr, captured, server_handle) = spawn_status_capture_server().await;

    let mut cmd = tokio::process::Command::new("/bin/true");
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let child = cmd.spawn().unwrap();
    let pid = child.id().unwrap();
    let process = crate::adapters::AgentProcess {
        child,
        pid,
        runtime: rb_schemas::AgentRuntime::ClaudeCode,
    };

    // Intentionally leave the sessions map EMPTY — simulates terminate_session
    // having already removed the entry before the natural-exit handler runs.
    let sessions: Arc<Mutex<HashMap<String, SessionHandle>>> = Arc::new(Mutex::new(HashMap::new()));
    let seq_counters = Arc::new(Mutex::new(HashMap::new()));
    let seq_timestamps = Arc::new(Mutex::new(HashMap::<String, Instant>::new()));
    let process_arc = Arc::new(Mutex::new(process));
    let session_uuid = uuid::Uuid::new_v4();
    let session_id = session_uuid.to_string();
    let tenant_id = TenantId::from(uuid::Uuid::new_v4());

    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let tenant_counts = Arc::new(std::sync::Mutex::new(HashMap::<TenantId, usize>::new()));
    let wait_handle = spawn_natural_exit_handler(
        session_id.clone(),
        tenant_id,
        Arc::clone(&process_arc),
        Arc::clone(&sessions),
        seq_counters,
        seq_timestamps,
        format!("http://{addr}"),
        reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap(),
        tx,
        Arc::clone(&tenant_counts),
    );

    wait_handle.await.expect("natural exit handler panicked");

    // No PATCH should have been sent because the session was "already gone".
    let g = captured.lock().await;
    assert!(
        g.is_empty(),
        "no HTTP calls expected when session is already removed; captured: {:?}",
        *g
    );

    server_handle.abort();
}

// ---------------------------------------------------------------------
// S2 / RUSAA-1812: crash recovery — SIGKILL → failed status (ADR-013 §4.4)
// ---------------------------------------------------------------------

/// Kills a running child process externally (SIGKILL) and verifies that the
/// natural-exit handler detects the crash, marks the session `failed`, and
/// surfaces an `error_kind=runtime_crashed` payload.
#[tokio::test(flavor = "current_thread")]
async fn crash_recovery_sigkill_marks_session_failed() {
    use std::process::Stdio;

    let (addr, captured, server_handle) = spawn_status_capture_server().await;

    // Spawn a long-running process so we can kill it mid-flight.
    let mut cmd = tokio::process::Command::new("/bin/sleep");
    cmd.arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let child = cmd.spawn().unwrap();
    let pid = child.id().unwrap();
    let process = crate::adapters::AgentProcess {
        child,
        pid,
        runtime: rb_schemas::AgentRuntime::ClaudeCode,
    };

    let sessions: Arc<Mutex<HashMap<String, SessionHandle>>> = Arc::new(Mutex::new(HashMap::new()));
    let seq_counters = Arc::new(Mutex::new(HashMap::new()));
    let seq_timestamps = Arc::new(Mutex::new(HashMap::<String, Instant>::new()));
    let process_arc = Arc::new(Mutex::new(process));
    let session_uuid = uuid::Uuid::new_v4();
    let session_id = session_uuid.to_string();
    let tenant_id = TenantId::from(uuid::Uuid::new_v4());

    {
        let mut map = sessions.lock().await;
        map.insert(
            session_id.clone(),
            make_stub_handle(Arc::clone(&process_arc), tenant_id),
        );
    }

    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let tenant_counts = Arc::new(std::sync::Mutex::new(HashMap::<TenantId, usize>::new()));
    // Pre-populate tenant count so the handler can decrement it.
    tenant_counts.lock().unwrap().insert(tenant_id, 1usize);

    let wait_handle = spawn_natural_exit_handler(
        session_id.clone(),
        tenant_id,
        Arc::clone(&process_arc),
        Arc::clone(&sessions),
        seq_counters,
        seq_timestamps,
        format!("http://{addr}"),
        reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap(),
        tx,
        Arc::clone(&tenant_counts),
    );

    // Simulate crash: SIGKILL the child from outside the supervisor.
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        let pid_i32 = i32::try_from(pid).expect("PID fits i32");
        kill(Pid::from_raw(pid_i32), Signal::SIGKILL).expect("SIGKILL must succeed");
    }

    wait_handle.await.expect("natural exit handler panicked");

    // The PATCH must report `status=failed` with the runtime_crashed error.
    let expected_path = format!("/internal/agent/sessions/{session_uuid}/status");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        {
            let g = captured.lock().await;
            if g.iter().any(|(rl, body)| {
                rl.starts_with("PATCH ")
                    && rl.contains(&expected_path)
                    && body.contains("\"status\":\"failed\"")
            }) {
                break;
            }
        }
        if tokio::time::Instant::now() > deadline {
            let g = captured.lock().await;
            panic!(
                "expected PATCH {expected_path} with status=failed; captured: {:?}",
                *g
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let g = captured.lock().await;
    let failed = g
        .iter()
        .find(|(rl, body)| {
            rl.starts_with("PATCH ")
                && rl.contains(&expected_path)
                && body.contains("\"status\":\"failed\"")
        })
        .expect("failed PATCH must be present");
    assert!(
        failed.1.contains("runtime_crashed"),
        "error payload must contain runtime_crashed; got: {}",
        failed.1
    );

    // Tenant count must be decremented back to 0 after the crash.
    let counts = tenant_counts.lock().unwrap();
    assert_eq!(
        counts.get(&tenant_id).copied().unwrap_or(0),
        0,
        "tenant session count must be 0 after crash"
    );

    server_handle.abort();
}

// per_tenant_limit_rejects_excess_sessions — moved to cap_tests.rs

// ---------------------------------------------------------------------
// MCP SHA pairing — mismatch detection logic, warn-only
// ---------------------------------------------------------------------

#[test]
fn mcp_sha_mismatch_detected_when_both_shas_known_and_differ() {
    let runner_sha = "aaaa1111bbbb2222cccc3333dddd4444eeee5555";
    let mcp_sha = "ffff6666aaaa7777bbbb8888cccc9999dddd0000";
    let mismatch = runner_sha != "unknown" && mcp_sha != "unknown" && runner_sha != mcp_sha;
    assert!(mismatch, "different known SHAs must be a mismatch");
}

#[test]
fn mcp_sha_no_mismatch_when_shas_are_equal() {
    let runner_sha = "aaaa1111bbbb2222cccc3333dddd4444eeee5555";
    let mcp_sha = runner_sha; // same SHA — no mismatch expected
    let mismatch = runner_sha != "unknown" && mcp_sha != "unknown" && runner_sha != mcp_sha;
    assert!(!mismatch, "identical SHAs must not mismatch");
}

#[test]
fn mcp_sha_no_mismatch_when_runner_sha_unknown() {
    let runner_sha = "unknown";
    let mcp_sha = "ffff6666aaaa7777bbbb8888cccc9999dddd0000";
    let mismatch = runner_sha != "unknown" && mcp_sha != "unknown" && runner_sha != mcp_sha;
    assert!(!mismatch, "unknown runner SHA must not trigger mismatch");
}

#[test]
fn mcp_sha_no_mismatch_when_mcp_sha_unknown() {
    let runner_sha = "aaaa1111bbbb2222cccc3333dddd4444eeee5555";
    let mcp_sha = "unknown";
    let mismatch = runner_sha != "unknown" && mcp_sha != "unknown" && runner_sha != mcp_sha;
    assert!(!mismatch, "unknown mcp SHA must not trigger mismatch");
}
