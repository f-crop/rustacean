use std::path::PathBuf;

use anyhow::Result;
use rb_kafka::{Consumer, ConsumerCfg};
use rb_schemas::AgentSessionCommand;

mod adapters;
mod consumer;
mod session;

fn validate_boot_env() -> Result<PathBuf> {
    let workspace_base = std::env::var("RB_AGENT_WORKSPACE_BASE")
        .unwrap_or_else(|_| "/data/workspaces".to_string());

    let workspace_path = PathBuf::from(&workspace_base);
    std::fs::create_dir_all(&workspace_path)?;

    Ok(workspace_path)
}

#[tokio::main]
async fn main() -> Result<()> {
    let workspace_base = validate_boot_env()?;
    let _guard = rb_tracing::init("rb-agent-runner")?;

    let consumer: Consumer<AgentSessionCommand> =
        Consumer::new(&ConsumerCfg::new("rb-agent-runner"))?;
    consumer.subscribe(&[consumer::TOPIC_AGENT_COMMANDS])?;

    tracing::info!(workspace_base = %workspace_base.display(), "rb-agent-runner starting");

    let control_api_base = std::env::var("RB_CONTROL_API_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());
    let http_client = reqwest::Client::new();

    let handle = consumer::spawn(consumer, workspace_base, control_api_base, http_client)?;

    shutdown_signal().await;
    tracing::info!("Shutdown signal received — stopping consumer");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_boot_env_creates_workspace_if_missing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace = temp_dir.path().join("nonexistent_workspace");
        // SAFETY: single-threaded test context; no concurrent env mutation
        unsafe { std::env::set_var("RB_AGENT_WORKSPACE_BASE", &workspace) };
        let result = validate_boot_env();
        assert!(result.is_ok());
        assert!(workspace.exists());
    }
}
