use axum::{
    extract::{Path, State},
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{require_session, AuthContext},
    state::AppState,
};

#[derive(Debug, Serialize)]
pub struct SessionResponse {
    pub session_id: String,
    pub tenant_id: String,
    pub runtime: String,
    pub status: String,
    pub input_prompt: String,
    pub workspace_path: String,
    pub trace_id: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
}

pub async fn get_session(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<SessionResponse>, AppError> {
    let session = require_session(&auth)?;
    let tenant_id = session.tenant_id;

    let row = sqlx::query(
        r#"
        SELECT 
            id, tenant_id, runtime, input_prompt, workspace_path,
            status, trace_id, created_at, started_at, ended_at
        FROM agent_sessions
        WHERE id = $1 AND tenant_id = $2
        "#,
    )
    .bind(session_id)
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Database)?;

    let row = row.ok_or(AppError::NotFound)?;

    Ok(Json(SessionResponse {
        session_id: row.get::<Uuid, _>("id").to_string(),
        tenant_id: row.get::<Uuid, _>("tenant_id").to_string(),
        runtime: row.get::<String, _>("runtime"),
        status: row.get::<String, _>("status"),
        input_prompt: row.get::<String, _>("input_prompt"),
        workspace_path: row.get::<String, _>("workspace_path"),
        trace_id: row.get::<Option<String>, _>("trace_id"),
        created_at: row
            .get::<chrono::DateTime<chrono::Utc>, _>("created_at")
            .to_rfc3339(),
        started_at: row
            .get::<Option<chrono::DateTime<chrono::Utc>>, _>("started_at")
            .map(|dt| dt.to_rfc3339()),
        ended_at: row
            .get::<Option<chrono::DateTime<chrono::Utc>>, _>("ended_at")
            .map(|dt| dt.to_rfc3339()),
    }))
}
