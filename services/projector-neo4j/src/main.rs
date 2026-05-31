use std::sync::Arc;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use rb_storage_neo4j::TenantGraph;

mod consumer;
mod writer;

#[derive(Parser)]
#[command(name = "projector-neo4j", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print compile-time build provenance as JSON and exit.
    BuildInfo,
}

fn validate_boot_env() -> Result<()> {
    let neo4j_password = std::env::var("NEO4J_PASSWORD").unwrap_or_default();
    if neo4j_password.is_empty() {
        anyhow::bail!(
            "projector-neo4j boot validation failed:\n  - NEO4J_PASSWORD: required but missing"
        );
    }
    Ok(())
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

    validate_boot_env()?;

    let _guard = rb_tracing::init("projector-neo4j")?;
    rb_metrics::spawn_metrics_server(rb_metrics::install_recorder("projector_neo4j")?);

    let neo4j_uri = std::env::var("NEO4J_URI").unwrap_or_else(|_| "bolt://neo4j:7687".to_owned());
    let neo4j_user = std::env::var("NEO4J_USER").unwrap_or_else(|_| "neo4j".to_owned());
    let neo4j_password = std::env::var("NEO4J_PASSWORD").context("NEO4J_PASSWORD is required")?;

    let graph = TenantGraph::connect(&neo4j_uri, &neo4j_user, &neo4j_password)
        .await
        .context("failed to connect to Neo4j")?;
    let graph = Arc::new(graph);

    tracing::info!("projector-neo4j starting");

    let handle = consumer::spawn(Arc::clone(&graph))?;

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
