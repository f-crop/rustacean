//! `POST /v1/agents/sessions` — create and launch an agent session (ADR-009 §6.4).
//!
//! # Prompt security (RUSAA-859)
//!
//! The full `initial_message` is forwarded to the runtime but **never stored
//! verbatim in the database**.  Only a ≤256-char Unicode preview is persisted
//! in `input_prompt_preview` (migration 011).

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    agents::session_runner::spawn_session_runner,
    agents::tool_dispatch::ControlApiToolDispatch,
    error::AppError,
    middleware::auth::{AuthContext, require_verified_session},
    routes::auth_oauth::token_store::PgTokenStore,
    state::{AppState, SessionHandle},
};

// ---------------------------------------------------------------------------
// Prompt security constants (RUSAA-859)
// ---------------------------------------------------------------------------

/// Maximum Unicode code points stored as a prompt preview in the DB.
const PROMPT_PREVIEW_MAX_CHARS: usize = 256;

/// Maximum byte length accepted for `initial_message` (64 KiB, per ADR-009 §4.1).
const INITIAL_MESSAGE_MAX_BYTES: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// Request / response
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    pub runtime_kind: String,
    pub model: String,
    #[serde(default)]
    pub system_prompt: String,
    /// Opening user message.  Capped at 64 KiB; only a ≤256-char preview is
    /// stored in the DB — the full text is forwarded to the runtime only.
    pub initial_message: String,
    #[serde(default = "default_budget")]
    pub token_budget: i64,
}

fn default_budget() -> i64 {
    100_000
}

#[derive(Debug, Serialize)]
pub struct CreateSessionResponse {
    pub session_id: Uuid,
    pub status: String,
    pub runtime_kind: String,
    pub model: String,
    pub token_budget: i64,
    pub created_at: chrono::DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Prompt preview helper (RUSAA-859)
// ---------------------------------------------------------------------------

/// Returns the first ≤`PROMPT_PREVIEW_MAX_CHARS` Unicode code points of `s`.
///
/// Counts by `char` (Unicode scalar value), not bytes, so we never split a
/// multibyte sequence and the DB CHECK constraint
/// `char_length(input_prompt_preview) <= 256` (migration 011) is always met.
fn prompt_preview(s: &str) -> String {
    s.chars().take(PROMPT_PREVIEW_MAX_CHARS).collect()
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/v1/agents/sessions",
    request_body = serde_json::Value,
    responses(
        (status = 202, description = "Session created"),
        (status = 400, description = "Invalid runtime_kind or fields"),
        (status = 401, description = "Authentication required"),
        (status = 429, description = "Process session cap reached"),
        (status = 503, description = "Runtime not configured"),
    ),
    tag = "agents"
)]
#[allow(clippy::too_many_lines)]
pub async fn create_session(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(req): Json<CreateSessionRequest>,
) -> Result<impl IntoResponse, AppError> {
    let session = require_verified_session(auth)?;

    if !matches!(req.runtime_kind.as_str(), "claude_code" | "open_code" | "pi") {
        return Err(AppError::InvalidInput);
    }

    if req.initial_message.trim().is_empty() {
        return Err(AppError::InvalidInput);
    }

    // Enforce 64 KiB cap on the incoming prompt (ADR-009 §4.1).
    if req.initial_message.len() > INITIAL_MESSAGE_MAX_BYTES {
        return Err(AppError::InvalidInput);
    }

    if req.token_budget <= 0 {
        return Err(AppError::InvalidInput);
    }

    let permit = state
        .agent_registry
        .try_acquire()
        .ok_or(AppError::SessionCapExceeded)?;

    let session_id = Uuid::new_v4();
    let now = Utc::now();

    // Derive the DB-safe preview BEFORE the INSERT.  Full prompt is kept in
    // `req.initial_message` for the runtime (Phase 2) but never written to DB.
    let preview = prompt_preview(&req.initial_message);

    // Dynamic query — agents schema not in sqlx offline cache yet (ADR-009 Phase 1).
    sqlx::query(
        r"
        INSERT INTO agents.agent_sessions
            (id, tenant_id, user_id, runtime_kind, model, system_prompt,
             status, token_budget, tokens_used, input_prompt_preview, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, 'created', $7, 0, $8, $9)
        ",
    )
    .bind(session_id)
    .bind(session.tenant_id)
    .bind(session.user_id)
    .bind(&req.runtime_kind)
    .bind(&req.model)
    .bind(&req.system_prompt)
    .bind(req.token_budget)
    .bind(preview)
    .bind(now)
    .execute(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("failed to insert agent_session: {e}");
        AppError::Internal(anyhow::anyhow!("DB insert failed"))
    })?;

    let handle = SessionHandle::new(
        session_id,
        session.tenant_id,
        session.user_id,
        req.runtime_kind.clone(),
        req.token_budget,
    );
    state.agent_registry.insert(handle.clone());

    // Spawn the runtime to execute the agent session (ADR-009 §6.4)
    let tool_dispatch = ControlApiToolDispatch::new(
        state.pool.clone(),
        state.qdrant.clone(),
        state.module_tree_cache.clone(),
    );

    // Build runtime-specific key based on runtime_kind
    let litellm_key = match req.runtime_kind.as_str() {
        "open_code" => state.config.litellm_open_code_key.clone(),
        "pi" => state.config.litellm_pi_key.clone(),
        _ => None,
    };

    // Build token store for claude_code runtime (OAuth-required)
    let token_store: Option<std::sync::Arc<dyn rb_agent_runtime::TokenStore>> =
        if req.runtime_kind == "claude_code" {
            state
                .config
                .claude_oauth_client_id
                .as_ref()
                .map(|client_id| {
                    std::sync::Arc::new(PgTokenStore::new(
                        state.pool.clone(),
                        state.http_client.clone(),
                        client_id.clone(),
                        60, // refresh lead seconds
                        state.token_cipher.clone(),
                    )) as std::sync::Arc<dyn rb_agent_runtime::TokenStore>
                })
        } else {
            None
        };

    spawn_session_runner(
        state.pool.clone(),
        state.agent_registry.clone(),
        std::sync::Arc::clone(&state.sse_bus),
        handle.clone(),
        req.runtime_kind.clone(),
        req.model.clone(),
        req.system_prompt.clone(),
        req.initial_message.clone(),
        req.token_budget,
        state.http_client.clone(),
        token_store,
        state.config.litellm_url.clone(),
        litellm_key,
        tool_dispatch,
    );

    tracing::info!(
        session_id = %session_id,
        runtime_kind = %req.runtime_kind,
        prompt_chars = req.initial_message.chars().count(),
        "agent session created"
    );

    let resp = CreateSessionResponse {
        session_id,
        status: "created".into(),
        runtime_kind: req.runtime_kind,
        model: req.model,
        token_budget: req.token_budget,
        created_at: now,
    };

    drop(permit);
    Ok((StatusCode::ACCEPTED, Json(resp)))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_budget_is_100k() {
        assert_eq!(default_budget(), 100_000);
    }

    #[test]
    fn valid_runtime_kinds() {
        for k in &["claude_code", "open_code", "pi"] {
            assert!(matches!(*k, "claude_code" | "open_code" | "pi"));
        }
    }

    #[test]
    fn invalid_runtime_kind_detected() {
        let k = "unknown";
        assert!(!matches!(k, "claude_code" | "open_code" | "pi"));
    }

    // --- Prompt preview tests (RUSAA-859) ---

    #[test]
    fn prompt_preview_short_string_unchanged() {
        assert_eq!(prompt_preview("Hello, world!"), "Hello, world!");
    }

    #[test]
    fn prompt_preview_exactly_256_chars_unchanged() {
        let s: String = "a".repeat(256);
        let preview = prompt_preview(&s);
        assert_eq!(preview.chars().count(), 256);
        assert_eq!(preview, s);
    }

    #[test]
    fn prompt_preview_truncates_at_256_chars() {
        let s: String = "x".repeat(1000);
        let preview = prompt_preview(&s);
        assert_eq!(preview.chars().count(), PROMPT_PREVIEW_MAX_CHARS);
    }

    #[test]
    fn prompt_preview_handles_multibyte_unicode() {
        // Each '🦀' is 4 bytes; 300 of them = 1200 bytes but 300 chars.
        let s: String = "🦀".repeat(300);
        let preview = prompt_preview(&s);
        assert_eq!(preview.chars().count(), PROMPT_PREVIEW_MAX_CHARS);
        assert!(std::str::from_utf8(preview.as_bytes()).is_ok());
    }

    #[test]
    fn prompt_preview_empty_string() {
        assert_eq!(prompt_preview(""), "");
    }

    #[test]
    fn initial_message_max_bytes_is_64kib() {
        assert_eq!(INITIAL_MESSAGE_MAX_BYTES, 65_536);
    }
}
