//! `AgentRuntime` trait — the interface every runtime adapter must implement.
//!
//! The trait is object-safe so host processes can store `Box<dyn AgentRuntime>`.
//! Tool callbacks are injected via `ToolDispatch` — the runtime crate never
//! imports `rb-query` (ADR-009 §1 reverse-dep constraint).

use async_trait::async_trait;
use uuid::Uuid;

use crate::{
    error::RuntimeError,
    event::SessionEvent,
};

// ---------------------------------------------------------------------------
// ToolDispatch — host-supplied callback
// ---------------------------------------------------------------------------

/// Implemented by the host process (control-api) to bridge tool calls from
/// the runtime adapter into `rb-query`.
///
/// This indirection keeps `rb-agent-runtime` free of a dependency on `rb-query`.
#[async_trait]
pub trait ToolDispatch: Send + Sync + 'static {
    /// Execute `tool_name` with `arguments` for `tenant_id`.
    ///
    /// Returns the tool result as a JSON value, or an error string.
    async fn call(
        &self,
        tenant_id: Uuid,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, String>;
}

// ---------------------------------------------------------------------------
// SessionContext — per-session inputs
// ---------------------------------------------------------------------------

/// Inputs required to start an agent session.
#[derive(Debug, Clone)]
pub struct SessionContext {
    pub session_id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub model: String,
    pub system_prompt: String,
    pub initial_message: String,
    pub token_budget: i64,
}

// ---------------------------------------------------------------------------
// RunOutcome — returned when a session terminates
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub tokens_used: i64,
    pub final_message: Option<String>,
}

// ---------------------------------------------------------------------------
// AgentRuntime trait
// ---------------------------------------------------------------------------

/// Adapter interface for an LLM runtime (Claude, OpenCode, Pi).
///
/// Each adapter connects to a different LLM provider; all share the same
/// event emission and tool-dispatch contract.
#[async_trait]
pub trait AgentRuntime: Send + Sync + 'static {
    /// Human-readable name for this runtime (used in logs and metrics).
    fn kind(&self) -> &'static str;

    /// Run the agent session until completion, cancellation, or budget exhaustion.
    ///
    /// Emits `SessionEvent`s via `on_event` (written to DB + EventBus by caller)
    /// and calls tools via `dispatch`.
    async fn run(
        &self,
        ctx: SessionContext,
        dispatch: &dyn ToolDispatch,
        on_event: &(dyn Fn(SessionEvent) + Send + Sync),
    ) -> Result<RunOutcome, RuntimeError>;

    /// Cancel an in-flight session (best-effort).
    async fn cancel(&self, session_id: Uuid) -> Result<(), RuntimeError>;
}
