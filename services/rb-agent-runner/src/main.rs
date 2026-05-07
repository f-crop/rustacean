use anyhow::{Context as _, Result};
use rb_agent_runner::{ConsumerContext, SessionManager, WorkspaceManager};
use rb_kafka::{Consumer, ConsumerCfg, Producer, ProducerCfg};
use sqlx::PgPool;
use std::sync::Arc;

mod adapter;
mod adapters;
mod consumer;
mod session_manager;
mod workspace;

use adapter::AdapterError;
use consumer::TOPIC_AGENT_COMMANDS;

fn validate_boot_env() -> Result<()> {
    let required_vars = [
        ("RB_CONTROL_API_URL", "URL for control-api MCP endpoint"),
        ("RB_DATABASE_URL", "PostgreSQL connection string"),
    ];

    let mut missing = Vec::new();
    for (var, desc) in &required_vars {
        if std::env::var(var).is_err() {
            missing.push(format!("  - {} ({})", var, desc));
        }
    }

    if !missing.is_empty() {
        anyhow::bail!(
            "rb-agent-runner boot validation failed:\n{}",
            missing.join("\n")
        );
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    validate_boot_env()?;

    let _guard = rb_tracing::init("rb-agent-runner")?;

    let db_pool = PgPool::connect(&std::env::var("RB_DATABASE_URL")?)
        .await
        .context("failed to connect to database")?;

    let consumer: Consumer<rb_schemas::AgentCommand> =
        Consumer::new(&ConsumerCfg::new("rb-agent-runner-grp"))?;
    consumer.subscribe(&[TOPIC_AGENT_COMMANDS])?;

    let event_producer = Arc::new(Producer::<rb_schemas::AgentEvent>::new(&ProducerCfg::default())?);

    let session_manager = Arc::new(SessionManager::new());
    let workspace_manager = Arc::new(WorkspaceManager::from_env());

    let control_api_url = std::env::var("RB_CONTROL_API_URL")?;

    let ctx = Arc::new(ConsumerContext {
        session_manager,
        workspace_manager: Arc::clone(&workspace_manager),
        db_pool,
        event_producer,
        control_api_url,
    });

    tracing::info!("rb-agent-runner starting");

    let consumer_handle = tokio::spawn(consumer::run(consumer, ctx));

    let gc_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(3600));
        loop {
            interval.tick().await;
            match workspace_manager.cleanup_expired().await {
                Ok(count) => {
                    if count > 0 {
                        tracing::info!("Cleaned up {} expired workspaces", count);
                    }
                }
                Err(e) => {
                    tracing::error!("Workspace cleanup failed: {}", e);
                }
            }
        }
    });

    shutdown_signal().await;
    tracing::info!("shutdown signal received");

    consumer_handle.abort();
    gc_handle.abort();

    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal;
    let ctrl_c = async {
        signal::ctrl_c().await.expect("failed to listen for Ctrl+C");
    };
    #[cfg(unix)]
    {
        let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to listen for SIGTERM");
        tokio::select! {
            () = ctrl_c => {},
            _ = sigterm.recv() => {},
        }
    }
    #[cfg(not(unix))]
    ctrl_c.await;
}
