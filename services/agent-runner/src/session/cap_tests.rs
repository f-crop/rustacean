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
/// Also asserts the tenant counter stays at MAX after the failed call (no counter leak).
#[tokio::test(flavor = "current_thread")]
async fn per_tenant_limit_rejects_excess_sessions() {
    use rb_schemas::{AgentRuntime, AgentSessionStart};
    let (addr, server_handle) = noop_server().await;
    let tmp = tempfile::tempdir().unwrap();
    let manager = make_manager(addr, &tmp);
    let tenant_id = TenantId::from(uuid::Uuid::new_v4());
    {
        let tc = manager.caps.tenant_counts();
        let mut counts = tc.lock().unwrap();
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
    // Counter must still be exactly MAX — not leaked upward (C1 regression).
    let tc = manager.caps.tenant_counts();
    let counts = tc.lock().unwrap();
    assert_eq!(
        counts.get(&tenant_id).copied().unwrap_or(0),
        super::caps::MAX_SESSIONS_PER_TENANT,
        "tenant counter must not be incremented on a rejected session"
    );
    server_handle.abort();
}

/// ADR-013 §6.3 integration smoke: the parse → redact pipeline (same two
/// steps `spawn_output_handlers` applies to every stdout line) strips a
/// JWT-bearing payload before it can reach any durable store.
///
/// Exercises `ClaudeCodeAdapter::parse_stdout_line` + `rb_auth::redact_with_token`
/// together — the same chain used in the live stdio bridge.
#[test]
fn stdout_pipeline_redacts_jwt_before_payload_stored() {
    use crate::adapters::RuntimeAdapter as _;

    // Three-segment base64url token matching the JWT shape the redactor targets.
    // Not a real credential — purely exercises the §6.3 redaction contract.
    #[allow(clippy::const_is_empty)]
    let fake_jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJzbW9rZS10ZXN0In0.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"; // gitleaks:allow

    // One claude stream-json line containing the JWT in the content field.
    let raw_line = format!(r#"{{"type":"text","content":"token={fake_jwt}"}}"#);

    let adapter = crate::adapters::claude_code::ClaudeCodeAdapter::new();
    let parsed = adapter
        .parse_stdout_line(&raw_line)
        .expect("stream-json line must parse to Some(ParsedLine)");

    let live_token = "live-session-token";
    let redacted = rb_auth::redact_with_token(&parsed.payload, Some(live_token));

    assert!(
        !redacted.contains(fake_jwt),
        "JWT must be redacted before the payload is stored; got: {redacted}"
    );
    assert!(
        !redacted.contains("eyJ"),
        "JWT header prefix must not appear after redaction; got: {redacted}"
    );
}

/// ADR-013 §6.3 relay path: `redact_with_token` applied to the full raw stdout
/// line strips JWTs before the line is forwarded to `relay_stdout_events` (and
/// from there to SSE / DB).  This mirrors the fix that moved the relay call
/// inside the `if let Some(parsed)` guard with a pre-redacted line.
#[test]
fn relay_path_redacts_jwt_before_sse_db() {
    // Three-segment base64url token — same shape as the Kafka-path smoke test.
    #[allow(clippy::const_is_empty)]
    let fake_jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJzbW9rZS10ZXN0In0.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"; // gitleaks:allow

    let raw_line = format!(r#"{{"type":"text","content":"token={fake_jwt}"}}"#);

    let live_token = "live-session-token";
    let redacted_line = rb_auth::redact_with_token(&raw_line, Some(live_token));

    assert!(
        !redacted_line.contains(fake_jwt),
        "JWT must be stripped from the full line before relay; got: {redacted_line}"
    );
    assert!(
        !redacted_line.contains("eyJ"),
        "JWT header prefix must not appear in the relayed line; got: {redacted_line}"
    );
}

/// Workspace traversal error must not touch the tenant counter (C1 regression:
/// early-return paths before caps.acquire must leave the counter unchanged).
#[tokio::test(flavor = "current_thread")]
async fn tenant_counter_not_leaked_on_workspace_error() {
    use rb_schemas::{AgentRuntime, AgentSessionStart};
    let (addr, server_handle) = noop_server().await;
    let tmp = tempfile::tempdir().unwrap();
    let manager = make_manager(addr, &tmp);
    let tenant_id = TenantId::from(uuid::Uuid::new_v4());
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    // Traversal path is rejected by safe_join before caps.acquire is ever called.
    let start = AgentSessionStart {
        runtime: AgentRuntime::Pi as i32,
        workspace_path: "../../etc/passwd".to_owned(),
        api_key: "test-key".to_owned(),
        initial_prompt: "hello".to_owned(),
    };
    let err = manager
        .start_session(&start, tenant_id, &uuid::Uuid::new_v4().to_string(), tx)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("Rejected workspace_path"),
        "expected workspace rejection error; got: {err}"
    );
    let tc = manager.caps.tenant_counts();
    let counts = tc.lock().unwrap();
    assert_eq!(
        counts.get(&tenant_id).copied().unwrap_or(0),
        0,
        "tenant counter must be 0 when session setup fails before caps.acquire"
    );
    server_handle.abort();
}

/// `TenantCountGuard` rolls back the per-tenant counter when dropped without
/// defuse (C1 regression: any setup-window `?` return must not leave the
/// tenant permanently blocked after `MAX_SESSIONS_PER_TENANT` failures).
#[tokio::test(flavor = "current_thread")]
async fn tenant_count_guard_rolls_back_on_drop() {
    let (addr, server_handle) = noop_server().await;
    let tmp = tempfile::tempdir().unwrap();
    let manager = make_manager(addr, &tmp);
    let tenant_id = TenantId::from(uuid::Uuid::new_v4());

    // Acquire: count goes from 0 → 1.
    let (_permit, guard) = manager
        .caps
        .acquire(tenant_id)
        .expect("fresh tenant must acquire");
    {
        let tc = manager.caps.tenant_counts();
        let counts = tc.lock().unwrap();
        assert_eq!(
            counts.get(&tenant_id).copied().unwrap_or(0),
            1,
            "count must be 1 immediately after acquire"
        );
    }

    // Drop without defuse: count must return to 0 (setup-failure rollback).
    drop(guard);
    {
        let tc = manager.caps.tenant_counts();
        let counts = tc.lock().unwrap();
        assert_eq!(
            counts.get(&tenant_id).copied().unwrap_or(0),
            0,
            "count must roll back to 0 when guard drops without defuse"
        );
    }

    server_handle.abort();
}
