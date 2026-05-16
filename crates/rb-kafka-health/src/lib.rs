#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]

mod config;
mod watcher;

pub use config::WatchdogConfig;
pub use watcher::{HealthyConsumer, KafkaHealthWatcher};
