use anyhow::Result;
use rb_kafka::{Consumer, ConsumerCfg};
use rb_schemas::AgentCommand;

mod adapters;
mod consumer;
mod error;
mod workspace;

fn validate_boot_env() -> Result<()> {
    let workspace_base = std::env::var("RB_WORKSPACE_BASE")
        .unwrap_or_else(|_| "/data/workspaces".to_owned());
    
    if workspace_base.is_empty() {
        anyhow::bail!(
            "rb-agent-runner boot validation failed:\n  \
             - RB_WORKSPACE_BASE is empty"
        );
    }
    
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    validate_boot_env()?;

    let _guard = rb_tracing::init("rb-agent-runner")?;

    tracing::info!("rb-agent-runner starting");

    let cmd_consumer: Consumer<AgentCommand> =
        Consumer::new(&ConsumerCfg::new("rb-agent-runner"))?;
    cmd_consumer.subscribe(&[consumer::TOPIC_AGENT_COMMANDS])?;

    tracing::info!(
        topic = consumer::TOPIC_AGENT_COMMANDS,
        "subscribed to agent commands topic"
    );

    let handle = tokio::spawn(consumer::run(cmd_consumer));

    shutdown_signal().await;
    tracing::info!("shutdown signal received — stopping consumer");
    handle.abort();

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install CTRL+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
