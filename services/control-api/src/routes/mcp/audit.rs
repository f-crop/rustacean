//! Audit-event helper for MCP tool calls.
//!
//! Every `tools/call` invocation writes one row to `audit.audit_events` with
//! the tool name and a SHA-256 hash of the raw arguments (never the arguments
//! themselves — ADR-009 §security model).

use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

pub(super) async fn write_tool_call_audit(
    pool: &PgPool,
    tenant_id: Uuid,
    actor_user_id: Option<Uuid>,
    tool_name: &str,
    args: &serde_json::Value,
    outcome: &str,
) {
    let args_sha256 = {
        let mut h = Sha256::new();
        h.update(args.to_string().as_bytes());
        format!("{:x}", h.finalize())
    };

    let result = sqlx::query(
        "INSERT INTO audit.audit_events \
         (event_id, tenant_id, actor_kind, actor_user_id, action, outcome, occurred_at, payload) \
         VALUES ($1, $2, 'mcp_client', $3, $4, $5, now(), $6) \
         ON CONFLICT (tenant_id, event_id) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(actor_user_id)
    .bind(format!("mcp.tools.call.{tool_name}"))
    .bind(outcome)
    .bind(serde_json::json!({ "tool": tool_name, "args_sha256": args_sha256 }))
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
}
