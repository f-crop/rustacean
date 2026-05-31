//! Admin v1 endpoints — operator-only, gated by `RB_ADMIN_TOKEN` bearer token.
//!
//! Mounted at `/api/admin/v1/` on the public listener; IP-restricted by the
//! Tailscale/Caddy reverse proxy (ADR-012 §S1.3 threat model).

pub mod audit_log;
pub mod bootstrap;
pub mod tenants;

use sqlx::PgPool;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Shared audit-log helper
// ---------------------------------------------------------------------------

/// Writes one `auth.admin_audit_log` row.
///
/// Failures are logged but **never** propagated — audit must not block the
/// response path. Every caller is responsible for invoking this on every code
/// path (ADR-012 §S1.6.1). The caller must never pass raw token material as
/// `payload_summary` (§S1.6.3).
#[allow(clippy::too_many_arguments)]
pub async fn write_audit_row(
    pool: &PgPool,
    actor: &str,
    action: &str,
    tenant_id: Option<Uuid>,
    target_user_id: Option<Uuid>,
    request_id: Uuid,
    ip: Option<&str>,
    user_agent: Option<&str>,
    payload_summary: &serde_json::Value,
    outcome: &str,
    error_class: Option<&str>,
) {
    let result = sqlx::query(
        "INSERT INTO auth.admin_audit_log \
         (actor, action, tenant_id, target_user_id, request_id, \
          ip, user_agent, payload_summary, outcome, error_class) \
         VALUES ($1, $2, $3, $4, $5, $6::inet, $7, $8, $9, $10)",
    )
    .bind(actor)
    .bind(action)
    .bind(tenant_id)
    .bind(target_user_id)
    .bind(request_id)
    .bind(ip)
    .bind(user_agent)
    .bind(payload_summary)
    .bind(outcome)
    .bind(error_class)
    .execute(pool)
    .await;

    if let Err(e) = result {
        tracing::error!(
            action, outcome, error = %e,
            "failed to write admin audit row"
        );
    }
}
