//! `rb-agent-runtime` — LLM agent runtime adapters for `RustBrain` (ADR-009 Phase 1).
//!
//! # Architecture
//!
//! This crate implements the `AgentRuntime` trait for three runtimes:
//! - `ClaudeCodeRuntime` — Anthropic Messages API via OAuth PKCE token
//! - `OpenCodeRuntime` — `LiteLLM` in-cluster proxy (OpenAI-compat) with virtual key
//! - `PiRuntime` — `LiteLLM` in-cluster proxy with Pi virtual key
//!
//! **Reverse-dep constraint (ADR-009 §1):** this crate MUST NOT depend on
//! `rb-query`. Tool callbacks are supplied by the host process via the
//! `ToolDispatch` trait.

#![allow(clippy::missing_errors_doc)]

mod adapters;
mod error;
mod event;
mod runtime;

pub use adapters::claude::TokenStore;
pub use adapters::{ClaudeCodeRuntime, OpenCodeRuntime, PiRuntime};
pub use error::RuntimeError;
pub use event::{EventEnvelope, SessionEvent};
pub use runtime::{AgentRuntime, RunOutcome, SessionContext, ToolDispatch};
