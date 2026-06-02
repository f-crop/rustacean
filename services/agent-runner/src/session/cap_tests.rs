//! Integration tests for per-tenant and per-node session caps (ADR-013 §4.3).

use std::time::Duration;

use rb_schemas::TenantId;

use super::SessionManager;

async fn noop_server() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let _ = sock
                    .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                    .await;
            });
        }
    });
    (addr, handle)
}

fn make_manager(addr: std::net::SocketAddr, tmp: &tempfile::TempDir) -> SessionManager {
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
    SessionManager::new(
        tmp.path().to_path_buf(),
        format!("http://{addr}"),
        reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap(),
        relay_sender,
        "test-mcp-sha".to_string(),
    )
}

/// `start_session` rejects a new session once the per-tenant cap is saturated.
#[tokio::test(flavor = "current_thread")]
async fn per_tenant_limit_rejects_excess_sessions() {
    use rb_schemas::{AgentRuntime, AgentSessionStart};
    let (addr, server_handle) = noop_server().await;
    let tmp = tempfile::tempdir().unwrap();
    let manager = make_manager(addr, &tmp);
    let tenant_id = TenantId::from(uuid::Uuid::new_v4());
    {
        let mut counts = manager.caps.tenant_counts().lock_owned().await;
        counts.insert(tenant_id, super::caps::MAX_SESSIONS_PER_TENANT);
    }
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let start = AgentSessionStart {
        runtime: AgentRuntime::Pi as i32,
        workspace_path: format!("cap-test-tenant-{}", uuid::Uuid::new_v4()),
        api_key: "test-key".to_owned(),
        initial_prompt: "hello".to_owned(),
    };
    let err = manager
        .start_session(&start, tenant_id, &uuid::Uuid::new_v4().to_string(), tx)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("rate_limit_exceeded"));
    assert!(err.to_string().contains("tenant"));
    server_handle.abort();
}
