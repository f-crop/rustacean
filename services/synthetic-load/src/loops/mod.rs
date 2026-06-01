//! Loop orchestrator — runs the three workload loops concurrently across the
//! tenant pool and applies health gating every `health_interval`.

pub mod agent;
pub mod ingestion;
pub mod query;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::client::ApiClient;
use crate::config::Config;
use crate::health::run_health_check;
use crate::report::DailySummary;
use crate::state::HarnessState;
use crate::tenant::{TenantRecord, force_delete, provision};

/// Minimum consecutive catastrophic failures before the harness exits.
const CATASTROPHIC_FAILURE_SECS: u64 = 300;

#[allow(clippy::too_many_lines)]
pub async fn run(
    config: Arc<Config>,
    state: Arc<Mutex<HarnessState>>,
    cancel: CancellationToken,
) -> Result<()> {
    // Build a shared plain HTTP client for admin operations.
    let admin_http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("synthetic-load/0.1 rustbrain-harness-admin")
        .build()?;

    // Ensure the tenant pool is populated before starting loops.
    ensure_tenant_pool(&config, &state, &admin_http).await?;

    // Record expected SHA from first healthy service.
    {
        let probe = ApiClient::new(&config.target_url)?;
        if let Some(sha) = probe.health_build_sha(&config.target_url).await {
            let mut s = state.lock().await;
            if s.expected_sha.is_none() {
                tracing::info!(sha = %sha, "captured expected build SHA");
                s.expected_sha = Some(sha);
            }
        }
    }

    // Spawn the three per-tenant loop tasks.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<crate::state::IterationOutcome>(256);

    spawn_loop_tasks(&config, &state, tx.clone(), cancel.clone()).await;

    // Health check timer and daily report timer.
    let mut health_ticker = tokio::time::interval(config.health_interval);
    let mut checkpoint_ticker = tokio::time::interval(Duration::from_secs(300));
    let mut daily_ticker = tokio::time::interval(Duration::from_secs(3600));
    let mut tenant_rotation_ticker = tokio::time::interval(Duration::from_secs(7200));

    let mut db_unavailable_since: Option<std::time::Instant> = None;

    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                tracing::info!("cancellation received; writing final summary");
                break;
            }

            outcome = rx.recv() => {
                let Some(outcome) = outcome else { break; };
                let mut s = state.lock().await;
                match outcome.loop_name {
                    "ingestion" => s.ingestion.record(outcome.ok),
                    "agent"     => s.agent.record(outcome.ok),
                    "query"     => {
                        s.query.record(outcome.ok);
                        if let Some(ms) = outcome.latency_ms {
                            s.record_query_latency(ms);
                        }
                    }
                    _ => {}
                }
                if let Some(tid) = outcome.trace_id {
                    if !outcome.ok {
                        s.last_failed_trace_id = Some(tid);
                    }
                }
                metrics::counter!("rb_synthetic_load_iterations_total",
                    "loop" => outcome.loop_name,
                    "outcome" => if outcome.ok { "ok" } else { "error" }
                ).increment(1);
            }

            _tick = health_ticker.tick() => {
                let (expected_sha, prom_url, service_urls) = {
                    let s = state.lock().await;
                    (s.expected_sha.clone(), config.prometheus_url.clone(), config.service_urls.clone())
                };
                let probe = match ApiClient::new(&config.target_url) {
                    Ok(c) => c,
                    Err(e) => { tracing::warn!("health probe client error: {e}"); continue; }
                };
                let result = run_health_check(
                    &probe,
                    &service_urls,
                    expected_sha.as_deref(),
                    prom_url.as_deref(),
                ).await;

                let check_failed = !result.failures.is_empty();
                {
                    let mut s = state.lock().await;
                    s.health.checks_total += 1;
                    if check_failed {
                        s.health.checks_failed += 1;
                    }
                    s.health.degradation_events += result.degradation_events.len() as u64;
                    s.health.drift_events += result.drift_events.len() as u64;
                }

                if check_failed {
                    // Track catastrophic-failure window (all services down).
                    if result.up == 0 {
                        db_unavailable_since.get_or_insert_with(std::time::Instant::now);
                    } else {
                        db_unavailable_since = None;
                    }
                    tracing::warn!(
                        failures = ?result.failures,
                        availability = result.availability_pct(),
                        "health check failed"
                    );
                } else {
                    db_unavailable_since = None;
                }

                // Exit on catastrophic failure.
                if let Some(since) = db_unavailable_since {
                    if since.elapsed().as_secs() > CATASTROPHIC_FAILURE_SECS {
                        tracing::error!("stack unreachable for >5 min — catastrophic failure exit");
                        cancel.cancel();
                        break;
                    }
                }
            }

            _tick = checkpoint_ticker.tick() => {
                let mut s = state.lock().await;
                if let Err(e) = s.save(&config.state_dir) {
                    tracing::warn!("checkpoint save failed: {e}");
                }
            }

            _tick = daily_ticker.tick() => {
                let s = state.lock().await;
                let summary = DailySummary::from_state(&s);
                if let Err(e) = summary.write(&config.state_dir) {
                    tracing::warn!("daily summary write failed: {e}");
                }
                tracing::info!(
                    verdict = %summary.verdict,
                    elapsed_hours = summary.elapsed_hours,
                    "daily summary written"
                );
            }

            _tick = tenant_rotation_ticker.tick() => {
                rotate_oldest_tenant(&config, &state, &admin_http, tx.clone(), cancel.clone()).await;
            }
        }
    }

    // Final summary on exit.
    {
        let s = state.lock().await;
        let summary = DailySummary::from_state(&s);
        if let Err(e) = summary.write(&config.state_dir) {
            tracing::warn!("final summary write failed: {e}");
        }
        tracing::info!(verdict = %summary.verdict, "final summary written");
    }

    Ok(())
}

/// Spawn one task per tenant per loop type.
async fn spawn_loop_tasks(
    config: &Arc<Config>,
    state: &Arc<Mutex<HarnessState>>,
    tx: tokio::sync::mpsc::Sender<crate::state::IterationOutcome>,
    cancel: CancellationToken,
) {
    let tenants = state.lock().await.tenants.clone();
    for tenant in tenants {
        let cfg = Arc::clone(config);
        let tx_ingest = tx.clone();
        let cancel_i = cancel.clone();
        let t = tenant.clone();
        tokio::spawn(async move {
            ingestion_worker(cfg, t, tx_ingest, cancel_i).await;
        });

        let cfg = Arc::clone(config);
        let tx_agent = tx.clone();
        let cancel_a = cancel.clone();
        let t = tenant.clone();
        tokio::spawn(async move {
            agent_worker(cfg, t, tx_agent, cancel_a).await;
        });

        let cfg = Arc::clone(config);
        let tx_query = tx.clone();
        let cancel_q = cancel.clone();
        let t = tenant.clone();
        tokio::spawn(async move {
            query_worker(cfg, t, tx_query, cancel_q).await;
        });
    }
}

async fn ingestion_worker(
    config: Arc<Config>,
    tenant: TenantRecord,
    tx: tokio::sync::mpsc::Sender<crate::state::IterationOutcome>,
    cancel: CancellationToken,
) {
    let password = TenantRecord::password_for_slot(tenant.slot);
    let Ok(mut client) = ApiClient::new(&config.target_url) else {
        return;
    };

    // Re-login on start; abort if credentials are rejected to avoid a silently broken worker.
    if crate::tenant::login(&mut client, &tenant.email, &password)
        .await
        .is_err()
    {
        tracing::warn!(tenant_id = %tenant.tenant_id, "ingestion worker: initial login failed; aborting");
        return;
    }

    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            () = tokio::time::sleep(config.ingestion_interval) => {
                let outcome = ingestion::run_once(&mut client, &tenant, &password).await;
                if tx.send(outcome).await.is_err() { break; }
            }
        }
    }
}

async fn agent_worker(
    config: Arc<Config>,
    tenant: TenantRecord,
    tx: tokio::sync::mpsc::Sender<crate::state::IterationOutcome>,
    cancel: CancellationToken,
) {
    let password = TenantRecord::password_for_slot(tenant.slot);
    let Ok(mut client) = ApiClient::new(&config.target_url) else {
        return;
    };

    if crate::tenant::login(&mut client, &tenant.email, &password)
        .await
        .is_err()
    {
        tracing::warn!(tenant_id = %tenant.tenant_id, "agent worker: initial login failed; aborting");
        return;
    }

    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            () = tokio::time::sleep(config.agent_interval) => {
                let outcome = agent::run_once(&mut client, &tenant, &password).await;
                if tx.send(outcome).await.is_err() { break; }
            }
        }
    }
}

async fn query_worker(
    config: Arc<Config>,
    tenant: TenantRecord,
    tx: tokio::sync::mpsc::Sender<crate::state::IterationOutcome>,
    cancel: CancellationToken,
) {
    let password = TenantRecord::password_for_slot(tenant.slot);
    let Ok(mut client) = ApiClient::new(&config.target_url) else {
        return;
    };

    if crate::tenant::login(&mut client, &tenant.email, &password)
        .await
        .is_err()
    {
        tracing::warn!(tenant_id = %tenant.tenant_id, "query worker: initial login failed; aborting");
        return;
    }

    let mut iteration: u64 = 0;
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            () = tokio::time::sleep(config.query_interval) => {
                let outcome = query::run_once(&mut client, &tenant, &password, iteration).await;
                if tx.send(outcome).await.is_err() { break; }
                iteration += 1;
            }
        }
    }
}

/// Public entry point for `main.rs` provision-only mode.
pub async fn ensure_tenant_pool_pub(
    config: &Config,
    state: &Arc<Mutex<HarnessState>>,
    admin_http: &reqwest::Client,
) -> Result<()> {
    ensure_tenant_pool(config, state, admin_http).await
}

async fn ensure_tenant_pool(
    config: &Config,
    state: &Arc<Mutex<HarnessState>>,
    _admin_http: &reqwest::Client,
) -> Result<()> {
    let (current_count, next_slot) = {
        let s = state.lock().await;
        (s.tenants.len(), s.next_slot)
    };

    let needed = config.tenant_count.saturating_sub(current_count);
    if needed == 0 {
        tracing::info!(count = current_count, "tenant pool already populated");
        return Ok(());
    }

    tracing::info!(needed = needed, "provisioning synthetic tenants");
    for i in 0..needed {
        let slot = next_slot + i;
        let mut client = ApiClient::new(&config.target_url)?;
        match provision(&mut client, slot, &config.database_url).await {
            Ok(record) => {
                tracing::info!(slot = slot, tenant_id = %record.tenant_id, "provisioned tenant");
                let mut s = state.lock().await;
                s.tenants.push(record);
                s.next_slot = slot + 1;
            }
            Err(e) => {
                tracing::error!(slot = slot, error = %e, "failed to provision tenant");
                return Err(e.context(format!("failed to provision tenant at slot {slot}")));
            }
        }
    }
    Ok(())
}

async fn rotate_oldest_tenant(
    config: &Arc<Config>,
    state: &Arc<Mutex<HarnessState>>,
    admin_http: &reqwest::Client,
    tx: tokio::sync::mpsc::Sender<crate::state::IterationOutcome>,
    cancel: CancellationToken,
) {
    let oldest = {
        let s = state.lock().await;
        s.tenants.first().cloned()
    };
    let Some(oldest) = oldest else {
        return;
    };

    tracing::info!(tenant_id = %oldest.tenant_id, slot = oldest.slot, "rotating oldest tenant");

    // Force-delete the oldest tenant.
    if let Err(e) = force_delete(
        admin_http,
        &config.target_url,
        &config.admin_token,
        oldest.tenant_id,
    )
    .await
    {
        tracing::warn!(tenant_id = %oldest.tenant_id, error = %e, "force-delete failed; keeping tenant");
        return;
    }

    // Remove from pool.
    {
        let mut s = state.lock().await;
        s.tenants.retain(|t| t.tenant_id != oldest.tenant_id);
    }

    // Provision a replacement.
    let next_slot = state.lock().await.next_slot;
    let mut client = match ApiClient::new(&config.target_url) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("client build failed during rotation: {e}");
            return;
        }
    };
    match provision(&mut client, next_slot, &config.database_url).await {
        Ok(record) => {
            tracing::info!(slot = next_slot, tenant_id = %record.tenant_id, "replacement tenant provisioned");
            {
                let mut s = state.lock().await;
                s.tenants.push(record.clone());
                s.next_slot = next_slot + 1;
            }

            // Spawn all three worker tasks for the new tenant.
            let cfg = Arc::clone(config);
            let tx_i = tx.clone();
            let cancel_i = cancel.clone();
            let t = record.clone();
            tokio::spawn(async move {
                ingestion_worker(cfg, t, tx_i, cancel_i).await;
            });

            let cfg = Arc::clone(config);
            let tx_a = tx.clone();
            let cancel_a = cancel.clone();
            let t = record.clone();
            tokio::spawn(async move {
                agent_worker(cfg, t, tx_a, cancel_a).await;
            });

            let cfg = Arc::clone(config);
            let tx_q = tx;
            let cancel_q = cancel;
            tokio::spawn(async move {
                query_worker(cfg, record, tx_q, cancel_q).await;
            });
        }
        Err(e) => {
            tracing::warn!(slot = next_slot, error = %e, "replacement tenant provision failed");
        }
    }
}
