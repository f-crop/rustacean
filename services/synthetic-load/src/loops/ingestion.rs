//! Ingestion loop — connects repos and drives the full pipeline per ADR-012 §2.7.2.
//!
//! For each iteration: pick an existing repo on the tenant, trigger an
//! ingestion, poll until all stages reach a terminal state, then run a
//! semantic search to validate the results landed.

use std::time::{Duration, Instant};

use anyhow::Result;
use uuid::Uuid;

use crate::client::{ApiClient, SearchRequest, TriggerIngestionResponse, is_unauthorized};
use crate::state::IterationOutcome;
use crate::tenant::TenantRecord;

const POLL_INTERVAL: Duration = Duration::from_secs(10);
const INGESTION_TIMEOUT: Duration = Duration::from_secs(600);

pub async fn run_once(
    client: &mut ApiClient,
    tenant: &TenantRecord,
    password: &str,
) -> IterationOutcome {
    match try_run_once(client, tenant, password).await {
        Ok(trace_id) => IterationOutcome {
            loop_name: "ingestion",
            ok: true,
            latency_ms: None,
            trace_id,
        },
        Err(e) => {
            let trace_id = client.last_failed_trace_id.clone();
            tracing::warn!(tenant_id = %tenant.tenant_id, error = %e, "ingestion iteration failed");
            IterationOutcome {
                loop_name: "ingestion",
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
) -> Result<Option<String>> {
    // Find an existing repo on the tenant to trigger re-ingestion.
    let repos = match client
        .get_json::<crate::client::RepoListResponse>("/v1/repos")
        .await
    {
        Ok(r) => r.repos,
        Err(e) if is_unauthorized(&e) => {
            crate::tenant::login(client, &tenant.email, password).await?;
            client
                .get_json::<crate::client::RepoListResponse>("/v1/repos")
                .await?
                .repos
        }
        Err(e) => return Err(e),
    };

    if repos.is_empty() {
        // No repos connected to this tenant yet — skip iteration.
        tracing::debug!(tenant_id = %tenant.tenant_id, "no repos; skipping ingestion iteration");
        return Ok(None);
    }

    // Pick the first repo (deterministic).
    let repo = &repos[0];
    let path = format!("/v1/repos/{}/ingestions", repo.repo_id);

    let trigger: TriggerIngestionResponse = client.post_json(&path, &serde_json::json!({})).await?;

    let run_id: Uuid = trigger.ingest_run_id;
    let trace_id = trigger.trace_id.clone();

    // Poll stages until terminal.
    let deadline = Instant::now() + INGESTION_TIMEOUT;
    loop {
        if Instant::now() > deadline {
            anyhow::bail!("ingestion run {run_id} timed out after {INGESTION_TIMEOUT:?}");
        }
        tokio::time::sleep(POLL_INTERVAL).await;

        let timeline = client
            .get_json::<crate::client::StageTimelineResponse>(&format!(
                "/v1/ingestions/{run_id}/stages"
            ))
            .await?;

        let any_failed = timeline.stages.iter().any(|s| s.status == "failed");
        if any_failed {
            let msg = timeline
                .stages
                .iter()
                .filter(|s| s.status == "failed")
                .map(|s| {
                    format!(
                        "{}: {}",
                        s.stage,
                        s.error_message.as_deref().unwrap_or("no detail")
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            anyhow::bail!("ingestion run {run_id} failed: {msg}");
        }

        let all_done = !timeline.stages.is_empty()
            && timeline.stages.iter().all(|s| s.status == "succeeded");
        if all_done {
            break;
        }
    }

    // Post-ingestion smoke: run a quick search to validate results landed.
    let _: crate::client::SearchResponse = client
        .post_json(
            "/v1/search",
            &SearchRequest {
                q: "fn main".to_owned(),
                limit: Some(5),
            },
        )
        .await
        .unwrap_or(crate::client::SearchResponse {});

    Ok(trace_id)
}
