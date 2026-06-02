use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
#[allow(dead_code)]
pub struct ChatSessionRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Option<Uuid>,
    pub runtime: String,
    pub status: String,
    pub trace_id: String,
    pub created_at: DateTime<Utc>,
    pub last_activity_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, sqlx::FromRow)]
#[allow(dead_code)]
pub struct ChatMessageRow {
    pub id: Uuid,
    pub session_id: Uuid,
    pub seq: i32,
    pub role: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Session helpers
// ---------------------------------------------------------------------------

pub async fn db_insert_chat_session(
    pool: &PgPool,
    id: Uuid,
    tenant_id: Uuid,
    user_id: Uuid,
    runtime: &str,
    trace_id: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO control.chat_sessions \
         (id, tenant_id, user_id, runtime, trace_id) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(runtime)
    .bind(trace_id)
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(|e| {
        tracing::error!("failed to insert chat_session: {e}");
        AppError::Internal(anyhow::anyhow!("DB insert failed"))
    })
}

pub async fn db_get_chat_session(
    pool: &PgPool,
    session_id: Uuid,
    tenant_id: Uuid,
) -> Result<ChatSessionRow, AppError> {
    sqlx::query_as::<_, ChatSessionRow>(
        "SELECT id, tenant_id, user_id, runtime, status, trace_id, \
                created_at, last_activity_at, ended_at \
         FROM control.chat_sessions \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(session_id)
    .bind(tenant_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB query failed: {e}")))?
    .ok_or(AppError::ChatSessionNotFound)
}

// ---------------------------------------------------------------------------
// Message helpers
// ---------------------------------------------------------------------------

pub async fn db_insert_chat_message(
    pool: &PgPool,
    id: Uuid,
    session_id: Uuid,
    tenant_id: Uuid,
    role: &str,
    body: &str,
) -> Result<i32, AppError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("DB txn begin: {e}")))?;

    // Acquire a row-level lock on the session so concurrent inserts on the same
    // session queue here instead of racing on the MAX(seq) subquery.
    let locked: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM control.chat_sessions \
         WHERE id = $1 AND tenant_id = $2 \
         FOR UPDATE",
    )
    .bind(session_id)
    .bind(tenant_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB lock: {e}")))?;

    if locked.is_none() {
        return Err(AppError::ChatSessionNotFound);
    }

    let (seq,): (i32,) = sqlx::query_as(
        r"
        INSERT INTO control.chat_messages (id, session_id, tenant_id, seq, role, body)
        SELECT $1, $2, $3,
               COALESCE((SELECT MAX(seq) FROM control.chat_messages WHERE session_id = $2), 0) + 1,
               $4, $5
        RETURNING seq
        ",
    )
    .bind(id)
    .bind(session_id)
    .bind(tenant_id)
    .bind(role)
    .bind(body)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        tracing::error!("failed to insert chat_message: {e}");
        AppError::Internal(anyhow::anyhow!("DB insert failed"))
    })?;

    sqlx::query(
        "UPDATE control.chat_sessions SET last_activity_at = now() WHERE id = $1",
    )
    .bind(session_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB activity update: {e}")))?;

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("DB txn commit: {e}")))?;

    Ok(seq)
}

pub async fn db_list_chat_messages(
    pool: &PgPool,
    session_id: Uuid,
    tenant_id: Uuid,
    limit: i64,
    after_seq: Option<i32>,
) -> Result<Vec<ChatMessageRow>, AppError> {
    if let Some(after) = after_seq {
        sqlx::query_as::<_, ChatMessageRow>(
            "SELECT id, session_id, seq, role, body, created_at \
             FROM control.chat_messages \
             WHERE session_id = $1 AND tenant_id = $2 AND seq > $3 \
             ORDER BY seq ASC LIMIT $4",
        )
        .bind(session_id)
        .bind(tenant_id)
        .bind(after)
        .bind(limit)
        .fetch_all(pool)
        .await
    } else {
        sqlx::query_as::<_, ChatMessageRow>(
            "SELECT id, session_id, seq, role, body, created_at \
             FROM control.chat_messages \
             WHERE session_id = $1 AND tenant_id = $2 \
             ORDER BY seq ASC LIMIT $3",
        )
        .bind(session_id)
        .bind(tenant_id)
        .bind(limit)
        .fetch_all(pool)
        .await
    }
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB query failed: {e}")))
}
