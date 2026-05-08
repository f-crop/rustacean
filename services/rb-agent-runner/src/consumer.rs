use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use metrics::counter;
use rb_kafka::{Consumer, EventEnvelope, RetryPolicy};
use rb_schemas::{AgentCommand, RuntimeKind};
use tokio::process::Child;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::adapters::{AdapterFactory, runtime_kind_as_str};
use crate::error::Result;
use crate::workspace::Workspace;

pub const TOPIC_AGENT_COMMANDS: &str = "rb.agent.commands";

struct RunnerCtx {
    active_sessions: Mutex<HashMap<Uuid, Child>>,
}

impl RunnerCtx {
    fn new() -> Self {
        Self {
            active_sessions: Mutex::new(HashMap::new()),
        }
    }
}

pub async fn run(consumer: Consumer<AgentCommand>) {
    let ctx = Arc::new(RunnerCtx::new());

    loop {
        match consumer.next().await {
            None => {
                tracing::info!("rb-agent-runner: stream ended");
                break;
            }
            Some(Err(e)) => {
                tracing::error!("rb-agent-runner: kafka error: {e}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Some(Ok(envelope)) => {
                let session_id = envelope.payload.session_id.clone();
                let tenant_id = envelope.tenant_id;

                match process_command(&ctx, &envelope).await {
                    Ok(()) => {
                        counter!("rb_agent_runner_total", "outcome" => "ok").increment(1);
                        if let Err(e) = consumer.commit(&envelope).await {
                            tracing::warn!(session_id, "rb-agent-runner: commit failed: {e}");
                        }
                    }
                    Err(e) => {
                        let attempt = envelope._meta.attempt + 1;
                        tracing::error!(
                            attempt,
                            session_id,
                            tenant_id = %tenant_id,
                            "rb-agent-runner: processing failed: {e:#}"
                        );
                        counter!("rb_agent_runner_total", "outcome" => "err").increment(1);

                        let policy = RetryPolicy::default();
                        if policy.is_terminal(attempt) {
                            tracing::warn!(
                                attempt,
                                session_id,
                                "rb-agent-runner: max retries exceeded"
                            );
                            counter!("rb_agent_runner_dlq_total").increment(1);
                            let _ = consumer.nack_to_dlq(&envelope, &format!("{e:#}")).await;
                        } else {
                            let delay = policy
                                .next_delay(attempt)
                                .unwrap_or(Duration::from_secs(1));
                            tokio::time::sleep(delay).await;
                        }
                    }
                }
            }
        }
    }
}

async fn process_command(ctx: &Arc<RunnerCtx>, envelope: &EventEnvelope<AgentCommand>) -> Result<()> {
    let cmd = &envelope.payload;
    let session_id = Uuid::parse_str(&cmd.session_id)?;
    let tenant_id = envelope.tenant_id;

    match cmd.payload.as_ref() {
        Some(rb_schemas::agent_command::Payload::StartSession(start)) => {
            let kind = RuntimeKind::try_from(start.runtime_kind)
                .unwrap_or(RuntimeKind::Unspecified);
            
            tracing::info!(
                session_id = %session_id,
                tenant_id = %tenant_id,
                runtime_kind = runtime_kind_as_str(kind),
                "processing StartSession command"
            );

            let adapter = AdapterFactory::create(kind)?;
            let workspace = Workspace::create(session_id, tenant_id.as_uuid()).await?;
            
            let api_key = std::env::var("RB_AGENT_API_KEY")
                .unwrap_or_else(|_| "dummy-key".to_string());

            let child = adapter.spawn(&workspace, session_id, &api_key).await?;

            {
                let mut sessions = ctx.active_sessions.lock().await;
                sessions.insert(session_id, child);
            }

            counter!("rb_agent_runner_sessions_started_total",
                "runtime_kind" => runtime_kind_as_str(kind)
            ).increment(1);

            tracing::info!(session_id = %session_id, "session started");
        }
        Some(rb_schemas::agent_command::Payload::StopSession(stop)) => {
            tracing::info!(session_id = %stop.session_id, "processing StopSession command");

            let stop_session_id = Uuid::parse_str(&stop.session_id)?;

            let mut sessions = ctx.active_sessions.lock().await;
            if let Some(mut child) = sessions.remove(&stop_session_id) {
                let _ = child.kill().await;
                tracing::info!(session_id = %stop_session_id, "session stopped");
            }
        }
        None => {}
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_constant_is_correct() {
        assert_eq!(TOPIC_AGENT_COMMANDS, "rb.agent.commands");
    }
}
