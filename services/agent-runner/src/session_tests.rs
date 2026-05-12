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

    let manager = SessionManager::new(
        tmp.path().to_path_buf(),
        format!("http://{addr}"),
        reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap(),
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
