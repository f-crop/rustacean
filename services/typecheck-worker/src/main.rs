use std::sync::Arc;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use rb_blob::store_from_env;
use rb_kafka::{Consumer, ConsumerCfg, Producer, ProducerCfg};
use rb_kafka_health::{KafkaHealthWatcher, WatchdogConfig};
use rb_schemas::{IngestRequest, IngestStatusEvent, TypecheckedItemEvent};

mod consumer;
mod helpers;
mod type_extractor;

#[derive(Parser)]
#[command(name = "typecheck-worker", version)]
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
    let blob_store = std::env::var("RB_BLOB_STORE").unwrap_or_else(|_| "filesystem".to_owned());
    if !matches!(blob_store.as_str(), "filesystem" | "s3") {
        anyhow::bail!(
            "typecheck-worker boot validation failed:\n  \
             - RB_BLOB_STORE={blob_store:?}: must be 'filesystem' or 's3'"
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

    let _guard = rb_tracing::init("typecheck-worker")?;
    let metrics_handle = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .context("failed to install Prometheus metrics recorder")?;
    metrics::gauge!(
        "rb_build_info",
        "service" => "typecheck_worker",
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

    let blob_store = store_from_env()
        .await
        .context("failed to init blob store")?;

    let typecheck_cfg = ConsumerCfg::new("typecheck-worker");
    let cmd_consumer: Consumer<IngestRequest> = Consumer::new(&typecheck_cfg)?;
    cmd_consumer.subscribe(&[consumer::TOPIC_TYPECHECK_COMMANDS])?;
    let cmd_consumer = KafkaHealthWatcher::wrap(
        cmd_consumer,
        &typecheck_cfg,
        &[consumer::TOPIC_TYPECHECK_COMMANDS.to_owned()],
        WatchdogConfig::default(),
    );

    let item_producer = Arc::new(Producer::<TypecheckedItemEvent>::new(
        &ProducerCfg::default(),
    )?);
    let status_producer = Arc::new(Producer::<IngestStatusEvent>::new(&ProducerCfg::default())?);

    tracing::info!("typecheck-worker starting");

    let handle = tokio::spawn(consumer::run(
        cmd_consumer,
        blob_store,
        item_producer,
        status_producer,
    ));

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
