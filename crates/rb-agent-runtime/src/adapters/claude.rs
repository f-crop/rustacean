//! `ClaudeCodeRuntime` — Anthropic Messages API adapter (ADR-009 §6.3).
//!
//! Uses an OAuth access token obtained via the PKCE flow at
//! `GET /v1/auth/oauth/claude/start`.  Token refresh is handled here;
//! token storage is delegated to the `TokenStore` trait so this crate
//! avoids a direct DB dependency.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::instrument;
use uuid::Uuid;

use crate::{
    error::RuntimeError,
    event::{
        MessagePayload, SessionCompletedPayload, SessionCreatedPayload, SessionEvent,
        SessionRunningPayload, SessionStartingPayload, ThinkingPayload,
        ToolCallPayload, ToolResultPayload,
    },
    runtime::{AgentRuntime, RunOutcome, SessionContext, ToolDispatch},
};

// ---------------------------------------------------------------------------
// TokenStore — host-supplied, keeps this crate DB-free
// ---------------------------------------------------------------------------

/// Retrieves and refreshes OAuth tokens on behalf of a (tenant, user).
#[async_trait]
pub trait TokenStore: Send + Sync + 'static {
    async fn access_token(&self, tenant_id: Uuid, user_id: Uuid) -> Result<String, RuntimeError>;
}

// ---------------------------------------------------------------------------
// Anthropic Messages API types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    system: String,
    messages: Vec<AnthropicMessage>,
    tools: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingConfig>,
}

#[derive(Debug, Serialize)]
struct ThinkingConfig {
    r#type: String,
    budget_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AnthropicMessage {
    role: String,
    content: AnthropicContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum AnthropicContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text { text: String },
    Thinking { thinking: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String, is_error: Option<bool> },
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    #[allow(dead_code)]
    id: String,
    content: Vec<ContentBlock>,
    usage: Usage,
    #[allow(dead_code)]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Usage {
    input_tokens: u32,
    output_tokens: u32,
}

// ---------------------------------------------------------------------------
// ClaudeCodeRuntime
// ---------------------------------------------------------------------------

const ANTHROPIC_MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct ClaudeCodeRuntime {
    http: reqwest::Client,
    token_store: Arc<dyn TokenStore>,
    cancel_map: Arc<dashmap::DashMap<Uuid, tokio::sync::watch::Sender<bool>>>,
}

impl ClaudeCodeRuntime {
    pub fn new(http: reqwest::Client, token_store: Arc<dyn TokenStore>) -> Self {
        Self {
            http,
            token_store,
            cancel_map: Arc::new(dashmap::DashMap::new()),
        }
    }
}

#[async_trait]
impl AgentRuntime for ClaudeCodeRuntime {
    fn kind(&self) -> &'static str {
        "claude_code"
    }

    #[instrument(skip(self, dispatch, on_event), fields(session_id = %ctx.session_id))]
    async fn run(
        &self,
        ctx: SessionContext,
        dispatch: &dyn ToolDispatch,
        on_event: &(dyn Fn(SessionEvent) + Send + Sync),
    ) -> Result<RunOutcome, RuntimeError> {
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        self.cancel_map.insert(ctx.session_id, cancel_tx);

        on_event(SessionEvent::Created(SessionCreatedPayload {
            runtime_kind: "claude_code".into(),
            model: ctx.model.clone(),
            token_budget: ctx.token_budget,
        }));

        let token = self.token_store
            .access_token(ctx.tenant_id, ctx.user_id)
            .await?;

        on_event(SessionEvent::Starting(SessionStartingPayload {
            runtime_kind: "claude_code".into(),
        }));

        let mut messages: Vec<AnthropicMessage> = vec![
            AnthropicMessage {
                role: "user".into(),
                content: AnthropicContent::Text(ctx.initial_message.clone()),
            },
        ];

        let mut total_tokens: i64 = 0;
        let mut last_assistant_text: Option<String> = None;
        let start = std::time::Instant::now();

        loop {
            if *cancel_rx.borrow() {
                self.cancel_map.remove(&ctx.session_id);
                return Err(RuntimeError::Cancelled);
            }

            if total_tokens >= ctx.token_budget {
                return Err(RuntimeError::BudgetExhausted {
                    used: total_tokens,
                    budget: ctx.token_budget,
                });
            }

            let req_body = AnthropicRequest {
                model: ctx.model.clone(),
                max_tokens: 8192,
                system: ctx.system_prompt.clone(),
                messages: messages.clone(),
                tools: vec![],
                thinking: None,
            };

            on_event(SessionEvent::Running(SessionRunningPayload {
                first_message_at: Utc::now(),
            }));

            let resp = self
                .http
                .post(ANTHROPIC_MESSAGES_URL)
                .header("x-api-key", &token)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("content-type", "application/json")
                .json(&req_body)
                .send()
                .await?;

            if !resp.status().is_success() {
                let status = resp.status().as_u16();
                let msg = resp.text().await.unwrap_or_default();
                return Err(RuntimeError::AnthropicApi { status, message: msg });
            }

            let api_resp: AnthropicResponse = resp.json().await?;
            total_tokens += i64::from(api_resp.usage.input_tokens + api_resp.usage.output_tokens);

            let mut assistant_blocks: Vec<ContentBlock> = vec![];
            let mut tool_uses: Vec<(String, String, serde_json::Value)> = vec![];

            for block in &api_resp.content {
                match block {
                    ContentBlock::Text { text } => {
                        last_assistant_text = Some(text.clone());
                        on_event(SessionEvent::Message(MessagePayload {
                            role: "assistant".into(),
                            content: text.clone(),
                            tokens: Some(api_resp.usage.output_tokens),
                        }));
                        assistant_blocks.push(block.clone());
                    }
                    ContentBlock::Thinking { thinking } => {
                        on_event(SessionEvent::Thinking(ThinkingPayload {
                            thinking: thinking.clone(),
                        }));
                        assistant_blocks.push(block.clone());
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        on_event(SessionEvent::ToolCall(ToolCallPayload {
                            tool_name: name.clone(),
                            tool_use_id: id.clone(),
                            arguments: input.clone(),
                        }));
                        tool_uses.push((id.clone(), name.clone(), input.clone()));
                        assistant_blocks.push(block.clone());
                    }
                    ContentBlock::ToolResult { .. } => {}
                }
            }

            messages.push(AnthropicMessage {
                role: "assistant".into(),
                content: AnthropicContent::Blocks(assistant_blocks),
            });

            if tool_uses.is_empty() {
                break;
            }

            let mut tool_result_blocks: Vec<ContentBlock> = vec![];
            for (tool_use_id, tool_name, args) in tool_uses {
                let t0 = std::time::Instant::now();
                let (content, is_error) = match dispatch
                    .call(ctx.tenant_id, &tool_name, &args)
                    .await
                {
                    Ok(v) => (v.to_string(), false),
                    Err(e) => (e, true),
                };
                let duration_ms = u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX);

                on_event(SessionEvent::ToolResult(ToolResultPayload {
                    tool_use_id: tool_use_id.clone(),
                    content: content.clone(),
                    is_error,
                    duration_ms,
                }));

                tool_result_blocks.push(ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error: Some(is_error),
                });
            }

            messages.push(AnthropicMessage {
                role: "user".into(),
                content: AnthropicContent::Blocks(tool_result_blocks),
            });
        }

        self.cancel_map.remove(&ctx.session_id);
        let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        on_event(SessionEvent::Completed(SessionCompletedPayload {
            tokens_used: total_tokens,
            duration_ms,
        }));

        Ok(RunOutcome {
            tokens_used: total_tokens,
            final_message: last_assistant_text,
        })
    }

    async fn cancel(&self, session_id: Uuid) -> Result<(), RuntimeError> {
        if let Some(entry) = self.cancel_map.get(&session_id) {
            let _ = entry.send(true);
        }
        Ok(())
    }
}
