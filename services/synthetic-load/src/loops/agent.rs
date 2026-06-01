//! Agent-execution loop — creates a session, polls until terminal, then deletes.
//!
//! ADR-012 §2.7.2: "POST /agents/:id/sessions → poll → terminate".

use std::time::{Duration, Instant};

use crate::client::{
    ApiClient, CreateSessionRequest, CreateSessionResponse, SessionDetail, is_unauthorized,
};
use crate::state::IterationOutcome;
use crate::tenant::TenantRecord;

const POLL_INTERVAL: Duration = Duration::from_secs(5);
const SESSION_TIMEOUT: Duration = Duration::from_secs(120);
const SYNTHETIC_PROMPT: &str = "List the files in the workspace and exit.";

pub async fn run_once(
    client: &mut ApiClient,
    tenant: &TenantRecord,
    password: &str,
) -> IterationOutcome {
    match try_run_once(client, tenant, password).await {
        Ok(trace_id) => IterationOutcome {
            loop_name: "agent",
            ok: true,
            latency_ms: None,
            trace_id,
        },
        Err(e) => {
            let trace_id = client.last_failed_trace_id.clone();
            tracing::warn!(tenant_id = %tenant.tenant_id, error = %e, "agent iteration failed");
            IterationOutcome {
                loop_name: "agent",
                ok: false,
                latency_ms: None,
                trace_id,
            }
        }
    }
}

async fn try_run_once(
    client: &mut ApiClient,
    tenant: &TenantRecord,
    password: &str,
) -> anyhow::Result<Option<String>> {
    // Create session.
    let resp: CreateSessionResponse = match client
        .post_json(
            "/v1/agents/sessions",
            &CreateSessionRequest {
                runtime: "claude_code".to_owned(),
                initial_prompt: SYNTHETIC_PROMPT.to_owned(),
            },
        )
        .await
    {
        Ok(r) => r,
        Err(e) if is_unauthorized(&e) => {
            crate::tenant::login(client, &tenant.email, password).await?;
            client
                .post_json(
                    "/v1/agents/sessions",
                    &CreateSessionRequest {
                        runtime: "claude_code".to_owned(),
                        initial_prompt: SYNTHETIC_PROMPT.to_owned(),
                    },
                )
                .await?
        }
        Err(e) => return Err(e),
    };

    let session_id = resp.session_id;

    // Poll until terminal or timeout.
    let deadline = Instant::now() + SESSION_TIMEOUT;
    let mut last_status = resp.status.clone();

    loop {
        if Instant::now() > deadline {
            // Best-effort cleanup before bailing.
            let _ = client
                .delete_ok(&format!("/v1/agents/sessions/{session_id}"))
                .await;
            anyhow::bail!(
                "session {session_id} did not reach terminal state within {SESSION_TIMEOUT:?}; last status: {last_status}"
            );
        }
        tokio::time::sleep(POLL_INTERVAL).await;

        let detail: SessionDetail = client
            .get_json(&format!("/v1/agents/sessions/{session_id}"))
            .await?;
        last_status.clone_from(&detail.status);

        if is_terminal(&detail.status) {
            break;
        }
    }

    // Delete (clean up) after completion.
    let _ = client
        .delete_ok(&format!("/v1/agents/sessions/{session_id}"))
        .await;

    Ok(None)
}

fn is_terminal(status: &str) -> bool {
    matches!(
        status,
        "completed" | "failed" | "cancelled" | "terminated" | "error"
    )
}
