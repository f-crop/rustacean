use crate::error::AppError;
use uuid::Uuid;

pub(super) struct NewAgentSession<'a> {
    pub(super) session_id: Uuid,
    pub(super) tenant_id: Uuid,
    pub(super) user_id: Uuid,
    pub(super) runtime: &'a str,
    pub(super) preview: &'a str,
    pub(super) workspace_rel: &'a str,
    pub(super) api_key_id: Uuid,
    pub(super) now: chrono::DateTime<chrono::Utc>,
}

pub(super) async fn db_insert_session_api_key(
    executor: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    api_key_id: Uuid,
    tenant_id: Uuid,
    user_id: Uuid,
    session_id: Uuid,
    key_hash: &str,
    scopes_json: &serde_json::Value,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO control.api_keys \
         (id, tenant_id, key_hash, name, scopes, created_by_user_id) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(api_key_id)
    .bind(tenant_id)
    .bind(key_hash)
    .bind(format!("agent-session-{session_id}"))
    .bind(scopes_json)
    .bind(user_id)
    .execute(executor)
    .await
    .map(|_| ())
    .map_err(|e| {
        tracing::error!("failed to insert session api_key: {e}");
        AppError::Internal(anyhow::anyhow!("DB insert failed"))
    })
}

pub(super) async fn db_insert_agent_session(
    executor: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    row: &NewAgentSession<'_>,
) -> Result<(), AppError> {
    sqlx::query(
        r"INSERT INTO agents.agent_sessions
            (id, tenant_id, user_id, runtime_kind, model, system_prompt,
             status, token_budget, tokens_used, input_prompt_preview,
             workspace_path, api_key_id, created_at)
          VALUES ($1, $2, $3, $4, 'n/a', '',
                  'pending', 100000, 0, $5, $6, $7, $8)",
    )
    .bind(row.session_id)
    .bind(row.tenant_id)
    .bind(row.user_id)
    .bind(row.runtime)
    .bind(row.preview)
    .bind(row.workspace_rel)
    .bind(row.api_key_id)
    .bind(row.now)
    .execute(executor)
    .await
    .map(|_| ())
    .map_err(|e| {
        tracing::error!("failed to insert agent_session: {e}");
        AppError::Internal(anyhow::anyhow!("DB insert failed"))
    })
}
