use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rb_kafka::{Consumer, ConsumerCfg};
use rb_schemas::AgentSessionCommand;

mod adapters;
mod consumer;
mod session;
mod workspace_gc;

#[derive(Parser)]
#[command(name = "rb-agent-runner", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print compile-time build provenance as JSON and exit.
    BuildInfo,
}

fn validate_boot_env() -> Result<PathBuf> {
    let workspace_base =
        std::env::var("RB_AGENT_WORKSPACE_BASE").unwrap_or_else(|_| "/data/workspaces".to_string());

    let workspace_path = PathBuf::from(&workspace_base);
    std::fs::create_dir_all(&workspace_path)?;

    Ok(workspace_path)
}

#[tokio::main]
async fn main() -> Result<()> {
    if let Some(Command::BuildInfo) = Cli::parse().command {
        let info = rb_build_info::get();
        println!(
            "{}",
            serde_json::json!({
                "sha": info.sha,
                "timestamp": info.timestamp,
                "dirty": info.dirty,
            })
        );
        return Ok(());
    }

    let workspace_base = validate_boot_env()?;
    let _guard = rb_tracing::init("rb-agent-runner")?;
    let metrics_handle = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .context("failed to install Prometheus metrics recorder")?;
    metrics::gauge!(
        "rb_build_info",
        "service" => "agent_runner",
        "git_sha" => rb_build_info::SHA,
        "version" => env!("CARGO_PKG_VERSION"),
    )
    .set(1.0);
    let metrics_port: u16 = std::env::var("RB_METRICS_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9091);
    tokio::spawn(async move {
        use axum::routing::get;
        let app = axum::Router::new().route(
            "/metrics",
            get(move || async move { metrics_handle.render() }),
        );
        let listener = tokio::net::TcpListener::bind(("0.0.0.0", metrics_port))
            .await
            .expect("metrics listener bind failed");
        tracing::info!(port = metrics_port, "metrics server listening");
        axum::serve(listener, app)
            .await
            .expect("metrics server error");
    });

    let consumer: Consumer<AgentSessionCommand> =
        Consumer::new(&ConsumerCfg::new("rb-agent-runner"))?;

    // Defense against the kafka-init -> agent-runner metadata race observed in
    // compose dev/UAT: even when `depends_on: kafka-init` blocks startup until
    // topic creation succeeds, the broker can take a beat to publish the new
    // topic in its metadata cache. Calling subscribe directly in that window
    // logs UnknownTopicOrPartition once and then the stream goes silent for up
    // to one `topic.metadata.refresh.interval.ms`. Bounded retry here turns
    // that silent stall into a loud, short, observable wait.
    consumer
        .wait_for_topics(
            &[consumer::TOPIC_AGENT_COMMANDS],
            std::time::Duration::from_secs(60),
        )
        .await
        .context("rb.agent.commands topic not visible to broker within 60s")?;

    consumer.subscribe(&[consumer::TOPIC_AGENT_COMMANDS])?;

    tracing::info!(workspace_base = %workspace_base.display(), "rb-agent-runner starting");

    let control_api_base = std::env::var("RB_CONTROL_API_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());

    // H6: Attach the shared secret header to every internal control-api callback
    // with timeouts to prevent hanging on slow/unresponsive control-api
    let http_client = {
        let mut default_headers = reqwest::header::HeaderMap::new();
        if let Ok(secret) = std::env::var("RB_INTERNAL_SECRET") {
            let val = reqwest::header::HeaderValue::from_str(&secret)
                .context("RB_INTERNAL_SECRET contains invalid header characters")?;
            default_headers.insert("x-internal-secret", val);
        } else {
            tracing::warn!(
                "RB_INTERNAL_SECRET not set — internal callbacks will be rejected by control-api"
            );
        }
        reqwest::Client::builder()
            .default_headers(default_headers)
            // H6: Add timeouts to prevent indefinite hangs
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .context("Failed to build HTTP client")?
    };

    let consumer_handle = consumer::spawn(consumer, workspace_base, control_api_base, http_client)?;

    shutdown_signal().await;
    tracing::info!("Shutdown signal received — terminating active sessions");
    consumer_handle.session_manager.terminate_all().await;
    tracing::info!("Terminating consumer");
    consumer_handle.handle.abort();

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {}
            Err(e) => {
                tracing::error!("Failed to install CTRL+C handler: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
            }
            Err(e) => {
                tracing::error!("Failed to install SIGTERM handler: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
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
