//! `POST /v1/github/webhook` — GitHub App webhook receiver (REQ-GH-06, ADR-005 §7).
//!
//! Order is load-bearing:
//!   1. Read required headers.
//!   2. HMAC-SHA256 verify the *raw* request body **before** any JSON parse —
//!      keeps the signed bytes byte-identical to what GitHub HMAC'd.
//!   3. Replay protection via `X-GitHub-Delivery` UUID in a 10-minute TTL cache
//!      (`ReplayCache::try_insert_new` is atomic, so concurrent re-deliveries
//!      collapse to one observable insert).
//!   4. Only then deserialize the body — and only into the typed payload that
//!      matches `X-GitHub-Event`.
//!   5. Apply small SQL updates inline (all <= 50 ms; PRD allows < 1 s response).
//!
//! This route does **not** sit behind the auth middleware: the HMAC is the
//! authenticator, and any non-trivial 401/403 path would let GitHub's delivery
//! retries pile up without need.
//!
//! No tenant data ever appears in the response body. We send an empty body
//! and rely on `tracing` for forensics.

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use rb_github::{
    GhError, InstallationEvent, InstallationPayload, InstallationReposPayload,
    InstallationRepositoriesEvent,
};
use sqlx::PgPool;

use crate::state::AppState;

/// `X-Hub-Signature-256: sha256=<hex>` — set by GitHub on every delivery.
const HEADER_SIGNATURE: &str = "X-Hub-Signature-256";
/// `X-GitHub-Delivery: <uuid>` — unique per delivery attempt.
const HEADER_DELIVERY: &str = "X-GitHub-Delivery";
/// `X-GitHub-Event: <event-name>` — top-level event type.
const HEADER_EVENT: &str = "X-GitHub-Event";

/// Receive a GitHub webhook delivery.
///
/// Returns:
/// * `200 OK` on a fully-handled `installation.{created,deleted,suspend,
///   unsuspend}` or `installation_repositories.{added,removed}` delivery,
///   or on a duplicate delivery suppressed by the replay cache.
/// * `202 Accepted` for unmodelled event types or unmodelled actions —
///   GitHub treats these as success and stops retrying.
/// * `400 Bad Request` if a required header is missing or the body fails
///   to deserialize after a valid signature (malformed JSON).
/// * `401 Unauthorized` if `X-Hub-Signature-256` is missing, malformed, or
///   does not match the body HMAC.
/// * `503 Service Unavailable` if the GitHub App is not configured on this
///   instance (`RB_GH_APP_ID` / `RB_GH_APP_PRIVATE_KEY` unset).
///
/// **Not exposed via `OpenAPI`**: this route is consumed only by GitHub's
/// webhook delivery service. Authentication is via HMAC, not session/key, so
/// publishing it in the schema would mislead human/agent integrators.
pub async fn github_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(gh) = state.gh.clone() else {
        return WebhookOutcome::AppNotConfigured.into_response();
    };

    let sig = match required_header(&headers, HEADER_SIGNATURE) {
        Ok(v) => v,
        Err(o) => return o.into_response(),
    };
    let delivery = match required_header(&headers, HEADER_DELIVERY) {
        Ok(v) => v,
        Err(o) => return o.into_response(),
    };
    let event = match required_header(&headers, HEADER_EVENT) {
        Ok(v) => v,
        Err(o) => return o.into_response(),
    };

    // 1. Verify signature against the raw body bytes BEFORE any JSON parse.
    if let Err(err) = gh.verify_webhook(&body, sig) {
        match err {
            GhError::BadSignatureFormat => {
                tracing::warn!(%delivery, %event, "github webhook: bad signature format");
            }
            GhError::SignatureMismatch => {
                tracing::warn!(%delivery, %event, "github webhook: signature mismatch");
            }
            other => {
                tracing::error!(error = %other, %delivery, %event, "github webhook: unexpected verify error");
            }
        }
        return WebhookOutcome::SignatureFailed.into_response();
    }

    // 2. Replay protection — atomic via moka entry().
    if !gh.replay_cache.try_insert_new(delivery).await {
        tracing::info!(%delivery, %event, "github webhook: replay suppressed");
        return WebhookOutcome::Ok.into_response();
    }

    // 3. Parse based on the event header.
    let outcome = match event {
        "installation" => match serde_json::from_slice::<InstallationEvent>(&body) {
            Ok(evt) => handle_installation(&state.pool, delivery, evt).await,
            Err(err) => {
                tracing::warn!(%delivery, error = %err, "github webhook: installation parse failed");
                WebhookOutcome::BadBody
            }
        },
        "installation_repositories" => {
            match serde_json::from_slice::<InstallationRepositoriesEvent>(&body) {
                Ok(evt) => handle_installation_repositories(&state.pool, delivery, evt).await,
                Err(err) => {
                    tracing::warn!(
                        %delivery,
                        error = %err,
                        "github webhook: installation_repositories parse failed"
                    );
                    WebhookOutcome::BadBody
                }
            }
        }
        _ => {
            tracing::info!(%delivery, %event, "github webhook: ignoring unmodelled event");
            WebhookOutcome::Accepted
        }
    };

    outcome.into_response()
}

/// Read a required header as a `&str`; convert any failure into a structured
/// 400 / 401 outcome with a tracing breadcrumb.
fn required_header<'h>(
    headers: &'h HeaderMap,
    name: &'static str,
) -> Result<&'h str, WebhookOutcome> {
    let Some(value) = headers.get(name) else {
        tracing::warn!(missing_header = name, "github webhook: missing header");
        // Signature absence is treated as 401; everything else as 400.
        return Err(if name == HEADER_SIGNATURE {
            WebhookOutcome::SignatureFailed
        } else {
            WebhookOutcome::MissingHeader
        });
    };
    value.to_str().map_err(|err| {
        tracing::warn!(
            header = name,
            error = %err,
            "github webhook: header is not valid UTF-8"
        );
        if name == HEADER_SIGNATURE {
            WebhookOutcome::SignatureFailed
        } else {
            WebhookOutcome::MissingHeader
        }
    })
}

// ---------------------------------------------------------------------------
// Outcome -> HTTP response
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum WebhookOutcome {
    Ok,
    Accepted,
    MissingHeader,
    BadBody,
    SignatureFailed,
    AppNotConfigured,
    InternalError,
}

impl IntoResponse for WebhookOutcome {
    fn into_response(self) -> Response {
        let status = match self {
            Self::Ok => StatusCode::OK,
            Self::Accepted => StatusCode::ACCEPTED,
            Self::MissingHeader | Self::BadBody => StatusCode::BAD_REQUEST,
            Self::SignatureFailed => StatusCode::UNAUTHORIZED,
            Self::AppNotConfigured => StatusCode::SERVICE_UNAVAILABLE,
            Self::InternalError => StatusCode::INTERNAL_SERVER_ERROR,
        };
        // Empty body — no tenant data ever leaks via the webhook responder
        // (ADR-005 §9.3).
        status.into_response()
    }
}

// ---------------------------------------------------------------------------
// `installation` event handlers (ADR-005 §7.5)
// ---------------------------------------------------------------------------

async fn handle_installation(
    pool: &PgPool,
    delivery: &str,
    evt: InstallationEvent,
) -> WebhookOutcome {
    match evt {
        InstallationEvent::Created(p) => handle_installation_created(pool, delivery, p).await,
        InstallationEvent::Deleted(p) => {
            update_installation_flag(pool, delivery, InstallationOp::Deleted, p).await
        }
        InstallationEvent::Suspend(p) => {
            update_installation_flag(pool, delivery, InstallationOp::Suspend, p).await
        }
        InstallationEvent::Unsuspend(p) => {
            update_installation_flag(pool, delivery, InstallationOp::Unsuspend, p).await
        }
        InstallationEvent::Other => {
            tracing::info!(%delivery, "github webhook: ignoring installation action");
            WebhookOutcome::Accepted
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum InstallationOp {
    Deleted,
    Suspend,
    Unsuspend,
}

impl InstallationOp {
    fn as_str(self) -> &'static str {
        match self {
            Self::Deleted => "deleted",
            Self::Suspend => "suspend",
            Self::Unsuspend => "unsuspend",
        }
    }

    fn sql(self) -> &'static str {
        match self {
            Self::Deleted => {
                "UPDATE control.github_installations \
                 SET deleted_at = now() \
                 WHERE github_installation_id = $1 AND deleted_at IS NULL"
            }
            Self::Suspend => {
                "UPDATE control.github_installations \
                 SET suspended_at = now() \
                 WHERE github_installation_id = $1 \
                   AND suspended_at IS NULL AND deleted_at IS NULL"
            }
            Self::Unsuspend => {
                "UPDATE control.github_installations \
                 SET suspended_at = NULL \
                 WHERE github_installation_id = $1 AND deleted_at IS NULL"
            }
        }
    }
}

/// On `installation.created`, the OAuth callback path (REQ-GH-04) is the
/// authoritative writer of the `github_installations` row — only it knows the
/// `tenant_id`. The webhook may legitimately fire before *or* after the
/// callback. We treat it as idempotent confirmation: if a row exists, clear
/// `deleted_at` and `suspended_at`; otherwise log and accept.
async fn handle_installation_created(
    pool: &PgPool,
    delivery: &str,
    payload: InstallationPayload,
) -> WebhookOutcome {
    let installation_id = payload.installation.id;
    let result = sqlx::query(
        "UPDATE control.github_installations \
         SET deleted_at = NULL, suspended_at = NULL \
         WHERE github_installation_id = $1",
    )
    .bind(installation_id)
    .execute(pool)
    .await;

    match result {
        Ok(res) => {
            tracing::info!(
                %delivery,
                installation_id,
                rows_updated = res.rows_affected(),
                "github webhook: installation.created processed"
            );
            WebhookOutcome::Ok
        }
        Err(err) => {
            tracing::error!(
                %delivery,
                installation_id,
                error = %err,
                "github webhook: installation.created sql failed"
            );
            WebhookOutcome::InternalError
        }
    }
}

async fn update_installation_flag(
    pool: &PgPool,
    delivery: &str,
    op: InstallationOp,
    payload: InstallationPayload,
) -> WebhookOutcome {
    let installation_id = payload.installation.id;
    match sqlx::query(op.sql())
        .bind(installation_id)
        .execute(pool)
        .await
    {
        Ok(res) => {
            tracing::info!(
                %delivery,
                installation_id,
                action = op.as_str(),
                rows_updated = res.rows_affected(),
                "github webhook: installation update applied"
            );
            WebhookOutcome::Ok
        }
        Err(err) => {
            tracing::error!(
                %delivery,
                installation_id,
                action = op.as_str(),
                error = %err,
                "github webhook: installation update failed"
            );
            WebhookOutcome::InternalError
        }
    }
}

// ---------------------------------------------------------------------------
// `installation_repositories` event handlers
// ---------------------------------------------------------------------------

async fn handle_installation_repositories(
    pool: &PgPool,
    delivery: &str,
    evt: InstallationRepositoriesEvent,
) -> WebhookOutcome {
    match evt {
        // PRD: connect is explicit per REQ-GH-04. v1 logs and acknowledges.
        InstallationRepositoriesEvent::Added(p) => {
            tracing::info!(
                %delivery,
                installation_id = p.installation.id,
                added = p.repositories_added.len(),
                "github webhook: installation_repositories.added (no auto-connect)"
            );
            WebhookOutcome::Ok
        }
        InstallationRepositoriesEvent::Removed(p) => archive_removed_repos(pool, delivery, p).await,
        InstallationRepositoriesEvent::Other => {
            tracing::info!(
                %delivery,
                "github webhook: ignoring installation_repositories action"
            );
            WebhookOutcome::Accepted
        }
    }
}

/// Mark each removed repo as `archived_at = now()` for the matching
/// `(installation_id, github_repo_id)` pair. Missing rows are simply not
/// updated — GitHub may report repos we never connected.
async fn archive_removed_repos(
    pool: &PgPool,
    delivery: &str,
    payload: InstallationReposPayload,
) -> WebhookOutcome {
    let installation_id = payload.installation.id;
    if payload.repositories_removed.is_empty() {
        tracing::info!(
            %delivery,
            installation_id,
            "github webhook: removed list empty, no-op"
        );
        return WebhookOutcome::Ok;
    }

    let github_repo_ids: Vec<i64> = payload.repositories_removed.iter().map(|r| r.id).collect();

    let result = sqlx::query(
        "UPDATE control.repos \
         SET archived_at = now() \
         FROM control.github_installations gi \
         WHERE control.repos.installation_id = gi.id \
           AND gi.github_installation_id = $1 \
           AND control.repos.github_repo_id = ANY($2) \
           AND control.repos.archived_at IS NULL",
    )
    .bind(installation_id)
    .bind(&github_repo_ids)
    .execute(pool)
    .await;

    match result {
        Ok(res) => {
            tracing::info!(
                %delivery,
                installation_id,
                requested = github_repo_ids.len(),
                rows_updated = res.rows_affected(),
                "github webhook: installation_repositories.removed processed"
            );
            WebhookOutcome::Ok
        }
        Err(err) => {
            tracing::error!(
                %delivery,
                installation_id,
                error = %err,
                "github webhook: installation_repositories.removed sql failed"
            );
            WebhookOutcome::InternalError
        }
    }
}
