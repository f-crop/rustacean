use std::sync::Arc;

use anyhow::Result;
use rdkafka::{
    ClientConfig,
    Message,
    consumer::{Consumer as _, StreamConsumer},
    producer::FutureProducer,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

mod session;
mod workspace_gc;

use session::SessionManager;
use workspace_gc::WorkspaceGc;

const TOPIC_COMMANDS: &str = "rb.agent.commands";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCommand {
    pub session_id: Uuid,
    pub tenant_id: Uuid,
    pub command_type: i32,
    pub runtime_kind: String,
    pub model: String,
    pub system_prompt: String,
    pub initial_message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvent {
    pub session_id: Uuid,
    pub event_type: String,
    pub payload: serde_json::Value,
}

fn validate_boot_env() -> Result<()> {
    let mut errors: Vec<String> = Vec::new();

    let workspace_base = std::env::var("RB_AGENT_WORKSPACE_BASE")
        .unwrap_or_else(|_| "/data/workspaces".to_owned());
    if workspace_base.is_empty() {
        errors.push("RB_AGENT_WORKSPACE_BASE cannot be empty".to_owned());
    }

    let gc_interval = std::env::var("RB_AGENT_GC_INTERVAL_MINUTES")
        .unwrap_or_else(|_| "360".to_owned());
    if gc_interval.parse::<u64>().is_err() {
        errors.push(format!(
            "RB_AGENT_GC_INTERVAL_MINUTES={gc_interval:?}: must be a positive integer"
        ));
    }

    let ttl_days = std::env::var("RB_AGENT_WORKSPACE_TTL_DAYS")
        .unwrap_or_else(|_| "7".to_owned());
    if ttl_days.parse::<u64>().is_err() {
        errors.push(format!(
            "RB_AGENT_WORKSPACE_TTL_DAYS={ttl_days:?}: must be a positive integer"
        ));
    }

    if !errors.is_empty() {
        anyhow::bail!(
            "agent-runner boot validation failed ({} error(s)):\n{}",
            errors.len(),
            errors.iter().map(|e| format!("  - {e}")).collect::<Vec<_>>().join("\n")
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    validate_boot_env()?;

    let _guard = rb_tracing::init("agent-runner")?;

    let workspace_base = std::env::var("RB_AGENT_WORKSPACE_BASE")
        .unwrap_or_else(|_| "/data/workspaces".to_owned());

    let session_manager = Arc::new(SessionManager::new(&workspace_base).await?);

    let gc = Arc::new(WorkspaceGc::new(&workspace_base).await?);
    gc.start();

    let bootstrap_servers = std::env::var("KAFKA_BOOTSTRAP_SERVERS")
        .unwrap_or_else(|_| "localhost:9092".to_owned());

    let consumer: StreamConsumer = ClientConfig::new()
        .set("bootstrap.servers", &bootstrap_servers)
        .set("group.id", "agent-runner")
        .set("enable.auto.commit", "false")
        .set("auto.offset.reset", "earliest")
        .create()?;
    
    consumer.subscribe(&[TOPIC_COMMANDS])?;

    let _producer: FutureProducer = ClientConfig::new()
        .set("bootstrap.servers", &bootstrap_servers)
        .create()?;

    tracing::info!(workspace_base = %workspace_base, "agent-runner starting");

    let handle = tokio::spawn(run_consumer(
        consumer,
        session_manager.clone(),
    ));

    shutdown_signal().await;
    tracing::info!("shutdown signal received — stopping agent-runner");
    handle.abort();

    Ok(())
}

async fn run_consumer(
    consumer: StreamConsumer,
    session_manager: Arc<SessionManager>,
) {
    loop {
        match consumer.recv().await {
            Ok(msg) => {
                let Some(payload) = msg.payload() else {
                    continue;
                };

                let cmd: AgentCommand = match serde_json::from_slice(payload) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("failed to parse command: {}", e);
                        continue;
                    }
                };

                tracing::info!(
                    session_id = %cmd.session_id,
                    command_type = cmd.command_type,
                    "received agent command"
                );

                match cmd.command_type {
                    1 => {
                        if let Err(e) = session_manager
                            .start_session(cmd.session_id, &cmd)
                            .await
                        {
                            tracing::error!(
                                session_id = %cmd.session_id,
                                error = %e,
                                "failed to start session"
                            );
                        }
                    }
                    2 => {
                        if let Err(e) = session_manager
                            .terminate_session(cmd.session_id)
                            .await
                        {
                            tracing::error!(
                                session_id = %cmd.session_id,
                                error = %e,
                                "failed to terminate session"
                            );
                        }
                    }
                    _ => {
                        tracing::warn!(
                            session_id = %cmd.session_id,
                            command_type = cmd.command_type,
                            "unknown command type"
                        );
                    }
                }

                if let Err(e) = consumer.commit_message(&msg, rdkafka::consumer::CommitMode::Async) {
                    tracing::error!("failed to commit offset: {}", e);
                }
            }
            Err(e) => {
                tracing::error!("consumer error: {}", e);
            }
        }
    }
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
