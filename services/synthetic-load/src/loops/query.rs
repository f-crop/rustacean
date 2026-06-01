//! Query loop — measures semantic search latency.
//!
//! ADR-012 §2.7.2: "Query-loop p95 latency ≤ 2 s".

use std::time::Instant;

use crate::client::{ApiClient, SearchRequest, SearchResponse, is_unauthorized};
use crate::state::IterationOutcome;
use crate::tenant::TenantRecord;

/// Search terms rotated across iterations to exercise the embedding cache
/// and cold-path lookups.
const SEARCH_QUERIES: &[&str] = &[
    "fn main",
    "error handling",
    "async function",
    "database connection",
    "HTTP handler",
    "configuration loading",
    "tenant isolation",
    "Kafka consumer",
    "metrics counter",
    "graceful shutdown",
];

pub async fn run_once(
    client: &mut ApiClient,
    tenant: &TenantRecord,
    password: &str,
    iteration: u64,
) -> IterationOutcome {
    #[allow(clippy::cast_possible_truncation)]
    let query = SEARCH_QUERIES[iteration as usize % SEARCH_QUERIES.len()];

    match try_run_once(client, tenant, password, query).await {
        Ok(latency_ms) => IterationOutcome {
            loop_name: "query",
            ok: true,
            latency_ms: Some(latency_ms),
            trace_id: None,
        },
        Err(e) => {
            let trace_id = client.last_failed_trace_id.clone();
            tracing::warn!(tenant_id = %tenant.tenant_id, error = %e, "query iteration failed");
            IterationOutcome {
                loop_name: "query",
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
    query: &str,
) -> anyhow::Result<u64> {
    let t0 = Instant::now();

    let _resp: SearchResponse = match client
        .post_json(
            "/v1/search",
            &SearchRequest {
                q: query.to_owned(),
                limit: Some(10),
            },
        )
        .await
    {
        Ok(r) => r,
        Err(e) if is_unauthorized(&e) => {
            crate::tenant::login(client, &tenant.email, password).await?;
            client
                .post_json(
                    "/v1/search",
                    &SearchRequest {
                        q: query.to_owned(),
                        limit: Some(10),
                    },
                )
                .await?
        }
        Err(e) => return Err(e),
    };

    #[allow(clippy::cast_possible_truncation)]
    Ok(t0.elapsed().as_millis() as u64)
}
