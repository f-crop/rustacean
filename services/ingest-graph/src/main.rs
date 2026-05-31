use std::sync::Arc;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use rb_blob::store_from_env;
use rb_kafka::{Consumer, ConsumerCfg, Producer, ProducerCfg};
use rb_kafka_health::{KafkaHealthWatcher, WatchdogConfig};
use rb_schemas::{GraphRelationEvent, IngestStatusEvent, TypecheckedItemEvent};

mod consumer;
mod extractor;
mod extractor_calls;

#[derive(Parser)]
#[command(name = "ingest-graph", version)]
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
            "ingest-graph boot validation failed:\n  \
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

    let _guard = rb_tracing::init("ingest-graph")?;
    rb_metrics::spawn_metrics_server(rb_metrics::install_recorder("ingest_graph")?);

    let blob_store = store_from_env()
        .await
        .context("failed to init blob store")?;

    let graph_cfg = ConsumerCfg::new("ingest-graph");
    let item_consumer: Consumer<TypecheckedItemEvent> = Consumer::new(&graph_cfg)?;
    item_consumer.subscribe(&[consumer::TOPIC_TYPECHECKED_ITEMS])?;
    let item_consumer = KafkaHealthWatcher::wrap(
        item_consumer,
        &graph_cfg,
        &[consumer::TOPIC_TYPECHECKED_ITEMS.to_owned()],
        WatchdogConfig::default(),
    );

    let relation_producer = Arc::new(Producer::<GraphRelationEvent>::new(&ProducerCfg::default())?);
    let embed_producer = Arc::new(Producer::<TypecheckedItemEvent>::new(
        &ProducerCfg::default(),
    )?);
    let status_producer = Arc::new(Producer::<IngestStatusEvent>::new(&ProducerCfg::default())?);

    tracing::info!("ingest-graph starting");

    let handle = tokio::spawn(consumer::run(
        item_consumer,
        blob_store,
        relation_producer,
        embed_producer,
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
