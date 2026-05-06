use std::sync::Arc;

use anyhow::{Context as _, Result};
use rb_blob::store_from_env;
use rb_kafka::{Consumer, ConsumerCfg, Producer, ProducerCfg};
use rb_schemas::{ExpandedFileEvent, IngestRequest, IngestStatusEvent};

mod archive;
mod consumer;
mod workspace;

fn validate_boot_env() -> Result<()> {
    let blob_store = std::env::var("RB_BLOB_STORE").unwrap_or_else(|_| "filesystem".to_owned());
    if !matches!(blob_store.as_str(), "filesystem" | "s3") {
        anyhow::bail!(
            "expand-worker boot validation failed:\n  \
             - RB_BLOB_STORE={blob_store:?}: must be 'filesystem' or 's3'"
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    validate_boot_env()?;

    let _guard = rb_tracing::init("expand-worker")?;

    let blob_store = store_from_env().await.context("failed to init blob store")?;

    let consumer: Consumer<IngestRequest> =
        Consumer::new(&ConsumerCfg::new("expand-worker"))?;
    consumer.subscribe(&[consumer::TOPIC_EXPAND_COMMANDS])?;

    let expanded_producer = Arc::new(Producer::<ExpandedFileEvent>::new(&ProducerCfg::default())?);
    let parse_producer = Arc::new(Producer::<IngestRequest>::new(&ProducerCfg::default())?);
    let status_producer = Arc::new(Producer::<IngestStatusEvent>::new(&ProducerCfg::default())?);

    tracing::info!("expand-worker starting");

    let handle = tokio::spawn(consumer::run(
        consumer,
        blob_store,
        expanded_producer,
        parse_producer,
        status_producer,
    ));

    shutdown_signal().await;
    tracing::info!("shutdown signal received — stopping consumer");
    handle.abort();

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
