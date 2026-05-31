use std::sync::Arc;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use rb_storage_neo4j::TenantGraph;
use rb_storage_pg::TenantPool;

mod consumer;
mod delete;

#[derive(Parser)]
#[command(name = "tombstoner", version)]
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
    let mut errors: Vec<String> = Vec::new();

    let db_url = std::env::var("DATABASE_URL").unwrap_or_default();
    if db_url.is_empty() {
        errors.push("DATABASE_URL: required but missing".to_owned());
    } else if !db_url.starts_with("postgres") {
        errors.push(format!(
            "DATABASE_URL: expected postgres DSN, got {db_url:?}"
        ));
    }

    let neo4j_password = std::env::var("NEO4J_PASSWORD").unwrap_or_default();
    if neo4j_password.is_empty() {
        errors.push("NEO4J_PASSWORD: required but missing".to_owned());
    }

    if !errors.is_empty() {
        anyhow::bail!(
            "tombstoner boot validation failed ({} error(s)):\n{}",
            errors.len(),
            errors
                .iter()
                .map(|e| format!("  - {e}"))
                .collect::<Vec<_>>()
                .join("\n")
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

    let _guard = rb_tracing::init("tombstoner")?;
    let metrics_handle = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .context("failed to install Prometheus metrics recorder")?;
    metrics::gauge!(
        "rb_build_info",
        "service" => "tombstoner",
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

    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;

    let pg = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .context("failed to connect to PostgreSQL")?;
    let pool = Arc::new(TenantPool::new(pg));

    let neo4j_uri = std::env::var("NEO4J_URI").unwrap_or_else(|_| "bolt://neo4j:7687".to_owned());
    let neo4j_user = std::env::var("NEO4J_USER").unwrap_or_else(|_| "neo4j".to_owned());
    let neo4j_password = std::env::var("NEO4J_PASSWORD").context("NEO4J_PASSWORD is required")?;

    let graph = TenantGraph::connect(&neo4j_uri, &neo4j_user, &neo4j_password)
        .await
        .context("failed to connect to Neo4j")?;
    let graph = Arc::new(graph);

    // Optional Qdrant REST endpoint; tombstoner skips Qdrant deletion when unset.
    let qdrant_url = std::env::var("QDRANT_URL").ok();

    tracing::info!("tombstoner starting");

    let handle = consumer::spawn(pool, graph, qdrant_url)?;

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
