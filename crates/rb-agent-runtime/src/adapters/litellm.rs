//! LiteLLM adapter — `OpenCodeRuntime` and `PiRuntime` (ADR-009 §6.3).
//!
//! Both use the LiteLLM in-cluster proxy which exposes an OpenAI-compatible
//! `/chat/completions` endpoint.  The only difference is the virtual key and
//! model name used per runtime kind.

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::RuntimeError,
    event::{
        MessagePayload, SessionCompletedPayload, SessionCreatedPayload, SessionEvent,
        SessionRunningPayload, SessionStartingPayload, ToolCallPayload,
        ToolResultPayload,
    },
    runtime::{AgentRuntime, RunOutcome, SessionContext, ToolDispatch},
};

// ---------------------------------------------------------------------------
// OpenAI-compatible types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    tools: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    max_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCallBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolCallBlock {
    id: String,
    r#type: String,
    function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    usage: Option<UsageStats>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ChatMessage,
    #[allow(dead_code)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UsageStats {
    total_tokens: u32,
}

// ---------------------------------------------------------------------------
// LiteLlmRuntime (shared core)
// ---------------------------------------------------------------------------

struct LiteLlmRuntime {
    kind: &'static str,
    http: reqwest::Client,
    base_url: String,
    virtual_key: String,
}

impl LiteLlmRuntime {
    async fn run_inner(
        &self,
        ctx: SessionContext,
        dispatch: &dyn ToolDispatch,
        on_event: &(dyn Fn(SessionEvent) + Send + Sync),
    ) -> Result<RunOutcome, RuntimeError> {
        on_event(SessionEvent::Created(SessionCreatedPayload {
            runtime_kind: self.kind.into(),
            model: ctx.model.clone(),
            token_budget: ctx.token_budget,
        }));

        on_event(SessionEvent::Starting(SessionStartingPayload {
            runtime_kind: self.kind.into(),
        }));

        let mut messages: Vec<ChatMessage> = vec![
            ChatMessage {
                role: "system".into(),
                content: Some(ctx.system_prompt.clone()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".into(),
                content: Some(ctx.initial_message.clone()),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        let mut total_tokens: i64 = 0;
        let mut last_assistant_text: Option<String> = None;
        let start = std::time::Instant::now();
        let chat_url = format!("{}/chat/completions", self.base_url);

        loop {
            if total_tokens >= ctx.token_budget {
                return Err(RuntimeError::BudgetExhausted {
                    used: total_tokens,
                    budget: ctx.token_budget,
                });
            }

            on_event(SessionEvent::Running(SessionRunningPayload {
                first_message_at: Utc::now(),
            }));

            let req = ChatRequest {
                model: ctx.model.clone(),
                messages: messages.clone(),
                tools: vec![],
                tool_choice: None,
                max_tokens: 4096,
            };

            let resp = self
                .http
                .post(&chat_url)
                .header("Authorization", format!("Bearer {}", self.virtual_key))
                .header("content-type", "application/json")
                .json(&req)
                .send()
                .await?;

            if !resp.status().is_success() {
                let status = resp.status().as_u16();
                let msg = resp.text().await.unwrap_or_default();
                return Err(RuntimeError::LiteLlmApi { status, message: msg });
            }

            let api_resp: ChatResponse = resp.json().await?;
            if let Some(usage) = &api_resp.usage {
                total_tokens += usage.total_tokens as i64;
            }

            let choice = api_resp.choices.into_iter().next().ok_or_else(|| {
                RuntimeError::internal("LiteLLM returned empty choices")
            })?;

            let msg = choice.message.clone();

            if let Some(text) = &msg.content {
                if !text.is_empty() {
                    last_assistant_text = Some(text.clone());
                    on_event(SessionEvent::Message(MessagePayload {
                        role: "assistant".into(),
                        content: text.clone(),
                        tokens: None,
                    }));
                }
            }

            messages.push(msg.clone());

            let tool_calls = msg.tool_calls.unwrap_or_default();
            if tool_calls.is_empty() {
                break;
            }

            for call in tool_calls {
                let args: serde_json::Value =
                    serde_json::from_str(&call.function.arguments).unwrap_or(serde_json::json!({}));

                on_event(SessionEvent::ToolCall(ToolCallPayload {
                    tool_name: call.function.name.clone(),
                    tool_use_id: call.id.clone(),
                    arguments: args.clone(),
                }));

                let t0 = std::time::Instant::now();
                let (content, is_error) =
                    match dispatch.call(ctx.tenant_id, &call.function.name, &args).await {
                        Ok(v) => (v.to_string(), false),
                        Err(e) => (e, true),
                    };
                let duration_ms = t0.elapsed().as_millis() as u64;

                on_event(SessionEvent::ToolResult(ToolResultPayload {
                    tool_use_id: call.id.clone(),
                    content: content.clone(),
                    is_error,
                    duration_ms,
                }));

                messages.push(ChatMessage {
                    role: "tool".into(),
                    content: Some(content),
                    tool_calls: None,
                    tool_call_id: Some(call.id),
                });
            }
        }

        let duration_ms = start.elapsed().as_millis() as u64;
        on_event(SessionEvent::Completed(SessionCompletedPayload {
            tokens_used: total_tokens,
            duration_ms,
        }));

        Ok(RunOutcome {
            tokens_used: total_tokens,
            final_message: last_assistant_text,
        })
    }
}

// ---------------------------------------------------------------------------
// OpenCodeRuntime
// ---------------------------------------------------------------------------

pub struct OpenCodeRuntime(LiteLlmRuntime);

impl OpenCodeRuntime {
    pub fn new(http: reqwest::Client, base_url: String, virtual_key: String) -> Self {
        Self(LiteLlmRuntime {
            kind: "open_code",
            http,
            base_url,
            virtual_key,
        })
    }
}

#[async_trait]
impl AgentRuntime for OpenCodeRuntime {
    fn kind(&self) -> &'static str { "open_code" }

    async fn run(
        &self,
        ctx: SessionContext,
        dispatch: &dyn ToolDispatch,
        on_event: &(dyn Fn(SessionEvent) + Send + Sync),
    ) -> Result<RunOutcome, RuntimeError> {
        self.0.run_inner(ctx, dispatch, on_event).await
    }

    async fn cancel(&self, _session_id: Uuid) -> Result<(), RuntimeError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// PiRuntime
// ---------------------------------------------------------------------------

pub struct PiRuntime(LiteLlmRuntime);

impl PiRuntime {
    pub fn new(http: reqwest::Client, base_url: String, virtual_key: String) -> Self {
        Self(LiteLlmRuntime {
            kind: "pi",
            http,
            base_url,
            virtual_key,
        })
    }
}

#[async_trait]
impl AgentRuntime for PiRuntime {
    fn kind(&self) -> &'static str { "pi" }

    async fn run(
        &self,
        ctx: SessionContext,
        dispatch: &dyn ToolDispatch,
        on_event: &(dyn Fn(SessionEvent) + Send + Sync),
    ) -> Result<RunOutcome, RuntimeError> {
        self.0.run_inner(ctx, dispatch, on_event).await
    }

    async fn cancel(&self, _session_id: Uuid) -> Result<(), RuntimeError> {
        Ok(())
    }
}
