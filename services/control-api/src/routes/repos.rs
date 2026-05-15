//! Repo management endpoints.
//! - `POST /v1/repos`          — Connect a GitHub repo to the tenant (REQ-GH-04).
//! - `GET  /v1/repos`          — List connected repos for the tenant (REQ-GH-07).
//! - `POST /v1/repos/{id}/ingest` — Trigger an ingestion run (REQ-GH-08).

use std::time::Duration;

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use chrono::{DateTime, Utc};
use rb_github::GhError;
use rb_kafka::EventEnvelope;
use rb_schemas::{IngestRequest, TenantId};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, require_verified_session},
    state::AppState,
};

// ---------------------------------------------------------------------------
// POST /v1/repos — request / response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct ConnectRepoRequest {
    /// Internal installation UUID (from the GitHub App install redirect).
    pub installation_id: Uuid,
    /// GitHub numeric repository ID (from the list-repos response).
    pub github_repo_id: i64,
    /// Default branch override. If omitted, the value is fetched from GitHub.
    pub default_branch: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ConnectRepoResponse {
    pub repo_id: Uuid,
    pub full_name: String,
    pub default_branch: String,
}

// ---------------------------------------------------------------------------
// GET /v1/repos — response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct RepoItem {
    pub repo_id: Uuid,
    pub full_name: String,
    pub default_branch: String,
    pub status: String,
    pub connected_by: Uuid,
    pub connected_at: DateTime<Utc>,
    pub installation_id: Uuid,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ConnectedReposResponse {
    pub repos: Vec<RepoItem>,
}

type RepoRow = (Uuid, String, String, String, Uuid, DateTime<Utc>, Uuid);

// ---------------------------------------------------------------------------
// POST /v1/repos/{id}/ingest — response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct TriggerIngestResponse {
    pub run_id: Uuid,
    pub repo_id: Uuid,
    pub status: String,
}

// ---------------------------------------------------------------------------
// POST /v1/repos
// ---------------------------------------------------------------------------

/// Connect a GitHub repository to the calling user's active tenant.
///
/// Verifies the installation belongs to the session tenant, confirms the repo
/// is accessible via GitHub's API, then inserts a `repos` row with
/// `status = 'connected'`.
#[utoipa::path(
    post,
    path = "/v1/repos",
    request_body = ConnectRepoRequest,
    responses(
        (status = 201, description = "Repository connected", body = ConnectRepoResponse),
        (status = 401, description = "Not authenticated or session expired"),
        (status = 403, description = "Email not verified"),
        (status = 404, description = "Installation not found or not owned by this tenant"),
        (status = 409, description = "Repository already connected (repo_already_connected)"),
        (status = 422, description = "Repository not accessible via installation (repo_not_accessible)"),
        (status = 503, description = "GitHub App not configured on this instance"),
    ),
    tag = "repos"
)]
pub async fn connect_repo(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(body): Json<ConnectRepoRequest>,
) -> Result<impl IntoResponse, AppError> {
    let session = require_verified_session(auth)?;

    let gh = state
        .gh_loader
        .current()
        .ok_or(AppError::GithubAppNotConfigured)?;

    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT github_installation_id FROM control.github_installations \
         WHERE id = $1 \
           AND tenant_id = $2 \
           AND deleted_at IS NULL \
           AND suspended_at IS NULL",
    )
    .bind(body.installation_id)
    .bind(session.tenant_id)
    .fetch_optional(&state.pool)
    .await?;

    let (numeric_installation_id,) = row.ok_or(AppError::NotFound)?;

    let repo_info = gh
        .fetch_repo(numeric_installation_id, body.github_repo_id)
        .await
        .map_err(|e| match e {
            GhError::ApiError { status: 404, .. } | GhError::ApiError { status: 403, .. } => {
                AppError::RepoNotAccessible
            }
            other => AppError::Internal(anyhow::anyhow!("{other}")),
        })?;

    let default_branch = body.default_branch.unwrap_or(repo_info.default_branch);

    let repo_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO control.repos \
         (id, tenant_id, installation_id, github_repo_id, full_name, default_branch, connected_by, status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'connected')",
    )
    .bind(repo_id)
    .bind(session.tenant_id)
    .bind(body.installation_id)
    .bind(body.github_repo_id)
    .bind(&repo_info.full_name)
    .bind(&default_branch)
    .bind(session.user_id)
    .execute(&state.pool)
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(ref dbe) = e {
            if dbe.constraint() == Some("repos_tenant_id_github_repo_id_key") {
                return AppError::RepoAlreadyConnected;
            }
        }
        AppError::Database(e)
    })?;

    tracing::info!(
        %repo_id,
        tenant_id = %session.tenant_id,
        github_repo_id = body.github_repo_id,
        full_name = %repo_info.full_name,
        "repo connected"
    );

    Ok((
        StatusCode::CREATED,
        Json(ConnectRepoResponse {
            repo_id,
            full_name: repo_info.full_name,
            default_branch,
        }),
    ))
}

// ---------------------------------------------------------------------------
// GET /v1/repos
// ---------------------------------------------------------------------------

/// List all connected repositories for the current session's tenant.
///
/// Soft-deleted repos (`archived_at IS NOT NULL`) are excluded.
/// Results are ordered by `connected_at DESC` (most recently connected first).
/// Requires a verified session.
#[utoipa::path(
    get,
    path = "/v1/repos",
    responses(
        (status = 200, description = "List of connected repos", body = ConnectedReposResponse),
        (status = 401, description = "Not authenticated or session expired"),
        (status = 403, description = "Email not verified"),
    ),
    tag = "repos"
)]
pub async fn list_repos(
    State(state): State<AppState>,
    auth: AuthContext,
) -> Result<impl IntoResponse, AppError> {
    let session = require_verified_session(auth)?;

    let rows: Vec<RepoRow> = sqlx::query_as(
        "SELECT id, full_name, default_branch, status, connected_by, connected_at, installation_id \
         FROM control.repos \
         WHERE tenant_id = $1 AND archived_at IS NULL \
         ORDER BY connected_at DESC",
    )
    .bind(session.tenant_id)
    .fetch_all(&state.pool)
    .await?;

    let repos = rows
        .into_iter()
        .map(
            |(
                repo_id,
                full_name,
                default_branch,
                status,
                connected_by,
                connected_at,
                installation_id,
            )| {
                RepoItem {
                    repo_id,
                    full_name,
                    default_branch,
                    status,
                    connected_by,
                    connected_at,
                    installation_id,
                }
            },
        )
        .collect();

    Ok(Json(ConnectedReposResponse { repos }))
}

// ---------------------------------------------------------------------------
// POST /v1/repos/{id}/ingest — REQ-GH-08
// ---------------------------------------------------------------------------

const CLONE_COMMANDS_TOPIC: &str = "rb.ingest.clone.commands";

const PIPELINE_STAGES: &[&str] = &[
    "clone",
    "expand",
    "parse",
    "typecheck",
    "extract",
    "embed",
    "project_pg",
    "project_neo4j",
    "project_qdrant",
];

/// Trigger an asynchronous ingestion run for a connected repository.
///
/// Returns 202 immediately; ingestion is processed asynchronously by the worker.
/// 404 if the repository does not exist or belongs to another tenant.
/// 409 if an ingestion run is already queued or running for this repo.
/// 503 if the Kafka producer is unavailable.
#[utoipa::path(
    post,
    path = "/v1/repos/{id}/ingest",
    params(
        ("id" = Uuid, Path, description = "Repository UUID (from POST /v1/repos)")
    ),
    responses(
        (status = 202, description = "Ingestion run queued", body = TriggerIngestResponse),
        (status = 401, description = "Not authenticated or session expired"),
        (status = 403, description = "Email not verified"),
        (status = 404, description = "Repository not found or belongs to another tenant"),
        (status = 409, description = "Ingestion run already in-flight (ingest_run_already_in_flight)"),
        (status = 503, description = "Kafka producer not available (kafka_not_configured, kafka_unavailable)"),
    ),
    tag = "repos"
)]
pub async fn trigger_ingest(
    State(state): State<AppState>,
    auth: AuthContext,
    axum::extract::Path(repo_id): axum::extract::Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let session = require_verified_session(auth)?;

    let producer = state
        .ingest_producer
        .as_ref()
        .ok_or(AppError::KafkaNotConfigured)?;

    // Probe broker reachability before touching the DB to avoid orphan rows.
    if !producer.check_ready(Duration::from_millis(500)).await {
        return Err(AppError::KafkaUnavailable);
    }

    let exists: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM control.repos \
         WHERE id = $1 AND tenant_id = $2 AND archived_at IS NULL",
    )
    .bind(repo_id)
    .bind(session.tenant_id)
    .fetch_optional(&state.pool)
    .await?;
    exists.ok_or(AppError::NotFound)?;

    let in_flight: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM control.ingestion_runs \
         WHERE repo_id = $1 AND tenant_id = $2 AND status IN ('queued', 'running') LIMIT 1",
    )
    .bind(repo_id)
    .bind(session.tenant_id)
    .fetch_optional(&state.pool)
    .await?;
    if in_flight.is_some() {
        return Err(AppError::IngestRunAlreadyInFlight);
    }

    let run_id = Uuid::new_v4();
    let event_id = Uuid::new_v4();
    let trace_id = rb_tracing::current_trace_id();

    // Build the Kafka envelope before opening the transaction (pure in-memory).
    let ingest_req = IngestRequest {
        tenant_id: session.tenant_id.to_string(),
        event_id: event_id.to_string(),
        source: "api".to_string(),
        payload: vec![],
        created_at_ms: chrono::Utc::now().timestamp_millis(),
        repo_id: repo_id.to_string(),
        ingest_run_id: run_id.to_string(),
        commit_sha: String::new(),
        branch: String::new(),
    };
    let envelope =
        EventEnvelope::new(TenantId::from(session.tenant_id), ingest_req).with_event_id(event_id);
    let partition_key = format!("{}.{}", session.tenant_id, repo_id);

    // Insert ingestion_run + pipeline_stage_runs in a transaction.
    // Do NOT commit until Kafka publish succeeds — rollback on publish failure
    // guarantees no orphan ingestion_runs rows.
    let mut txn = state.pool.begin().await?;

    sqlx::query(
        "INSERT INTO control.ingestion_runs \
         (id, tenant_id, repo_id, status, requested_by, trace_id) \
         VALUES ($1, $2, $3, 'queued', $4, $5)",
    )
    .bind(run_id)
    .bind(session.tenant_id)
    .bind(repo_id)
    .bind(session.user_id)
    .bind(&trace_id)
    .execute(&mut *txn)
    .await?;

    for stage in PIPELINE_STAGES {
        sqlx::query(
            "INSERT INTO control.pipeline_stage_runs \
             (id, ingestion_run_id, stage) \
             VALUES ($1, $2, $3)",
        )
        .bind(Uuid::new_v4())
        .bind(run_id)
        .bind(*stage)
        .execute(&mut *txn)
        .await?;
    }

    // Publish before committing; rollback on broker failure.
    if let Err(e) = producer
        .publish(CLONE_COMMANDS_TOPIC, partition_key.as_bytes(), envelope)
        .await
    {
        txn.rollback().await.ok();
        return Err(AppError::KafkaPublish(e));
    }

    txn.commit().await?;

    tracing::info!(
        %run_id,
        %repo_id,
        tenant_id = %session.tenant_id,
        "ingestion run queued and dispatched to clone stage"
    );

    Ok((
        StatusCode::ACCEPTED,
        Json(TriggerIngestResponse {
            run_id,
            repo_id,
            status: "queued".to_owned(),
        }),
    ))
}

// ---------------------------------------------------------------------------
// Tests (extracted to repos_tests.rs to stay under the 600-line file cap)
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "repos_tests.rs"]
mod tests;
