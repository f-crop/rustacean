use std::sync::Arc;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use rb_storage_pg::TenantPool;

use projector_pg::spawn;

#[derive(Parser)]
#[command(name = "projector-pg", version)]
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
    let db_url = std::env::var("DATABASE_URL").unwrap_or_default();
    if db_url.is_empty() {
        anyhow::bail!(
            "projector-pg boot validation failed:\n  - DATABASE_URL: required but missing"
        );
    }
    if !db_url.starts_with("postgres") {
        anyhow::bail!(
            "projector-pg boot validation failed:\n  - DATABASE_URL: expected postgres DSN, got {db_url:?}"
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

    let _guard = rb_tracing::init("projector-pg")?;
    rb_metrics::spawn_metrics_server(rb_metrics::install_recorder("projector_pg")?);

    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;

    let pg = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await
        .context("failed to connect to PostgreSQL")?;
    let pool = TenantPool::new(pg);
    let pool = Arc::new(pool);

    tracing::info!("projector-pg starting");

    let handle = spawn(pool)?;

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
