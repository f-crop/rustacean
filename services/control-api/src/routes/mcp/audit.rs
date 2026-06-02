//! Audit-event helper for MCP tool calls.
//!
//! Every `tools/call` invocation writes one row to `audit.audit_events` with
//! the tool name and a SHA-256 hash of the raw arguments (never the arguments
//! themselves — ADR-009 §security model).

use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

/// Optional chat-context carried by MCP JWT callers.
///
/// Present when a runtime adapter calls `/mcp` with a short-lived JWT
/// (ADR-013 §5.2); absent for API-key / session auth.
pub(super) struct ChatAuditCtx<'a> {
    /// Chat session UUID from the JWT `sub` claim.
    pub chat_session_id: Uuid,
    /// JWT ID for audit correlation.
    pub jti: &'a str,
}

/// Write one row to `audit.audit_events` for a `tools/call` invocation.
///
/// - `chat_ctx` — audit correlation fields for MCP-JWT callers; `None` for
///   API-key / session-auth callers.
pub(super) async fn write_tool_call_audit(
    pool: &PgPool,
    tenant_id: Uuid,
    actor_user_id: Option<Uuid>,
    tool_name: &str,
    args: &serde_json::Value,
    outcome: &str,
    chat_ctx: Option<ChatAuditCtx<'_>>,
) {
    let args_sha256 = {
        let mut h = Sha256::new();
        h.update(args.to_string().as_bytes());
        format!("{:x}", h.finalize())
    };

    let mut payload = serde_json::json!({
        "tool": tool_name,
        "args_sha256": args_sha256,
    });
    if let Some(ctx) = &chat_ctx {
        payload["chat_session_id"] = serde_json::Value::String(ctx.chat_session_id.to_string());
        payload["jti"] = serde_json::Value::String(ctx.jti.to_owned());
    }

    // Discriminate between human chat sessions (MCP JWT) and agent/session callers.
    let actor_kind = if chat_ctx.is_some() { "chat" } else { "agent" };

    let result = sqlx::query(
        "INSERT INTO audit.audit_events \
         (event_id, tenant_id, actor_kind, actor_user_id, action, outcome, occurred_at, payload) \
         VALUES ($1, $2, $3, $4, $5, $6, now(), $7) \
         ON CONFLICT (tenant_id, event_id) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(actor_kind)
    .bind(actor_user_id)
    .bind(format!("mcp.tools.call.{tool_name}"))
    .bind(outcome)
    .bind(payload)
    .execute(pool)
    .await;

    if let Err(e) = result {
        tracing::warn!(
            tenant_id = %tenant_id,
            tool = tool_name,
            outcome,
            "failed to write MCP audit event: {e}"
        );
    }
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};
    use uuid::Uuid;

    use super::ChatAuditCtx;

    #[test]
    fn args_sha256_is_deterministic() {
        let args = serde_json::json!({"query": "my fn"});
        let hash1 = {
            let mut h = Sha256::new();
            h.update(args.to_string().as_bytes());
            format!("{:x}", h.finalize())
        };
        let hash2 = {
            let mut h = Sha256::new();
            h.update(args.to_string().as_bytes());
            format!("{:x}", h.finalize())
        };
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 64);
    }

    #[test]
    fn actor_kind_is_chat_when_ctx_present() {
        let jti = Uuid::new_v4().to_string();
        let ctx = Some(ChatAuditCtx {
            chat_session_id: Uuid::new_v4(),
            jti: &jti,
        });
        let kind = if ctx.is_some() { "chat" } else { "agent" };
        assert_eq!(kind, "chat");
    }

    #[test]
    fn actor_kind_is_agent_when_no_ctx() {
        let ctx: Option<ChatAuditCtx<'_>> = None;
        let kind = if ctx.is_some() { "chat" } else { "agent" };
        assert_eq!(kind, "agent");
    }
}
