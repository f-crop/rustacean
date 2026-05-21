use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::{Json, extract::State, response::IntoResponse};
use chrono::{DateTime, Utc};
use serde::Serialize;
use utoipa::{OpenApi as _, ToSchema};

// ---------------------------------------------------------------------------
// GET /health/build — compile-time build provenance
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct BuildInfoResponse {
    /// Compile-time git SHA baked into the binary.  `"unknown"` when built
    /// without `RB_BUILD_SHA` in the environment (e.g. ad-hoc local builds).
    pub sha: &'static str,
    /// Commit timestamp captured at compile time (`RB_BUILD_TIMESTAMP`).
    pub timestamp: &'static str,
    /// `"true"` when the working tree had uncommitted changes at build time.
    pub dirty: &'static str,
}

/// Compile-time build provenance — SHA, timestamp, and dirty flag.
///
/// Public / unauthenticated. Returns commit SHA only (board decision 2026-05-21).
/// Used by CI smoke gate and the dev-stack watcher to detect stale images.
#[utoipa::path(
    get,
    path = "/health/build",
    responses(
        (status = 200, description = "Build provenance", body = BuildInfoResponse)
    ),
    tag = "health"
)]
pub async fn build_info() -> Json<BuildInfoResponse> {
    let info = rb_build_info::get();
    Json(BuildInfoResponse {
        sha: info.sha,
        timestamp: info.timestamp,
        dirty: info.dirty,
    })
}

// ---------------------------------------------------------------------------
// GET /v1/_version — build identity
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct VersionResponse {
    pub git_sha: String,
    pub built_at: String,
}

/// Build identity — SHA and timestamp baked in at image build time.
///
/// Public / unauthenticated. Returns `"unknown"` for fields not set during
/// the docker build (e.g. local `docker build` without `--build-arg`).
#[utoipa::path(
    get,
    path = "/v1/_version",
    responses(
        (status = 200, description = "Build identity", body = VersionResponse)
    ),
    tag = "health"
)]
pub async fn version() -> Json<VersionResponse> {
    Json(VersionResponse {
        git_sha: std::env::var("GIT_SHA").unwrap_or_else(|_| "unknown".to_string()),
        built_at: std::env::var("BUILT_AT").unwrap_or_else(|_| "unknown".to_string()),
    })
}

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, Scope, require_verified_session},
    openapi::ApiDoc,
    state::AppState,
};

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

/// Simple status envelope used by `/ready`.
#[derive(Serialize, ToSchema)]
pub struct ProbeResponse {
    pub status: &'static str,
}

// ---------------------------------------------------------------------------
// GET /health — per-store liveness (REQ-DP-07)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct StoreStatuses {
    pub postgres: &'static str,
    pub neo4j: &'static str,
    pub qdrant: &'static str,
    pub kafka: &'static str,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    /// Overall status: `"ok"` when all stores are reachable, `"degraded"` otherwise.
    pub status: &'static str,
    pub stores: StoreStatuses,
}

/// Liveness probe with per-store connectivity status.
///
/// Returns 200 in all cases (even when stores are degraded) so load-balancers
/// do not kill the process — callers inspect `status` for fine-grained health.
/// Public / unauthenticated.
#[utoipa::path(
    get,
    path = "/health",
    responses(
        (status = 200, description = "Per-store health status", body = HealthResponse)
    ),
    tag = "health"
)]
pub async fn health_check(State(state): State<AppState>) -> Json<HealthResponse> {
    let (postgres, neo4j, qdrant, kafka) = tokio::join!(
        check_postgres(&state),
        check_neo4j(&state),
        check_qdrant(&state),
        check_kafka_liveness(&state),
    );

    let overall = if postgres == "ok" && neo4j != "error" && qdrant != "error" && kafka != "error" {
        "ok"
    } else {
        "degraded"
    };

    Json(HealthResponse {
        status: overall,
        stores: StoreStatuses {
            postgres,
            neo4j,
            qdrant,
            kafka,
        },
    })
}

async fn check_postgres(state: &AppState) -> &'static str {
    match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await
    {
        Ok(_) => "ok",
        Err(_) => "error",
    }
}

async fn check_neo4j(state: &AppState) -> &'static str {
    let Some(uri) = &state.neo4j_uri else {
        return "unknown";
    };
    // bolt:// is raw TCP — extract host:port and do a TCP connect probe.
    let addr = uri
        .strip_prefix("bolt://")
        .unwrap_or(uri.as_str())
        .trim_end_matches('/');
    let addr = if addr.contains(':') {
        addr.to_owned()
    } else {
        format!("{addr}:7687")
    };
    match tokio::time::timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(addr)).await {
        Ok(Ok(_)) => "ok",
        _ => "error",
    }
}

async fn check_qdrant(state: &AppState) -> &'static str {
    let Some(base) = state.config.qdrant_url.as_deref() else {
        return "unknown";
    };
    let url = format!("{}/healthz", base.trim_end_matches('/'));
    match tokio::time::timeout(Duration::from_secs(2), state.http_client.get(&url).send()).await {
        Ok(Ok(resp)) if resp.status().is_success() => "ok",
        _ => "error",
    }
}

fn check_kafka_liveness(state: &AppState) -> impl std::future::Future<Output = &'static str> {
    let last_ms = state
        .kafka_consistency
        .last_event_at_ms
        .load(Ordering::Relaxed);
    let age_secs = age_from_ms(last_ms);
    std::future::ready(if last_ms == 0 {
        "unknown"
    } else if age_secs < 300 {
        "ok"
    } else {
        "error"
    })
}

// ---------------------------------------------------------------------------
// GET /v1/health/consistency — admin-only Kafka lag (REQ-DP-07)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct StoreConsistency {
    pub lag_messages: u64,
    pub last_event_at: Option<DateTime<Utc>>,
    /// `healthy` (<30 s), `degraded` (30–300 s), `stale` (>300 s or never).
    pub status: &'static str,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ConsistencyStores {
    pub kafka: StoreConsistency,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ConsistencyResponse {
    pub checked_at: DateTime<Utc>,
    pub stores: ConsistencyStores,
}

/// Kafka consistency metrics (admin only).
///
/// Reports consumer lag and time since last event for each data-plane store.
/// Admin-only because these metrics expose internal pipeline internals.
///
/// Requires an `Admin`-scoped API key **or** an active session with at least
/// the `admin` tenant role.
#[utoipa::path(
    get,
    path = "/v1/health/consistency",
    responses(
        (status = 200, description = "Consistency metrics", body = ConsistencyResponse),
        (status = 401, description = "Not authenticated or session expired"),
        (status = 403, description = "Insufficient role or scope"),
    ),
    tag = "health"
)]
pub async fn consistency_check(
    State(state): State<AppState>,
    auth: AuthContext,
) -> Result<impl IntoResponse, AppError> {
    check_admin(&state.pool, auth).await?;

    let last_ms = state
        .kafka_consistency
        .last_event_at_ms
        .load(Ordering::Relaxed);
    let lag = state.kafka_consistency.lag_records.load(Ordering::Relaxed);

    let last_event_at: Option<DateTime<Utc>> = if last_ms > 0 {
        DateTime::from_timestamp_millis(last_ms)
    } else {
        None
    };

    let age_secs = age_from_ms(last_ms);
    let status = derive_status(last_ms, age_secs);

    Ok(Json(ConsistencyResponse {
        checked_at: Utc::now(),
        stores: ConsistencyStores {
            kafka: StoreConsistency {
                lag_messages: lag,
                last_event_at,
                status,
            },
        },
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns seconds since the epoch-ms timestamp, or `i64::MAX` when ms == 0.
fn age_from_ms(last_ms: i64) -> i64 {
    if last_ms == 0 {
        return i64::MAX;
    }
    let now_ms = Utc::now().timestamp_millis();
    (now_ms - last_ms).max(0) / 1000
}

/// Derives the consistency status from last-event age.
/// healthy < 30 s, degraded 30–300 s, stale > 300 s or never.
fn derive_status(last_ms: i64, age_secs: i64) -> &'static str {
    if last_ms == 0 || age_secs > 300 {
        "stale"
    } else if age_secs >= 30 {
        "degraded"
    } else {
        "healthy"
    }
}

/// Verifies the caller holds admin rights (API key with Admin scope, or session
/// with owner/admin tenant role).  Mirrors the pattern in `routes/audit`.
async fn check_admin(pool: &sqlx::PgPool, auth: AuthContext) -> Result<(), AppError> {
    match auth {
        AuthContext::ApiKey(info) => {
            if info.scopes.contains(&Scope::Admin) {
                Ok(())
            } else {
                Err(AppError::InsufficientScope)
            }
        }
        other => {
            let session = require_verified_session(other)?;
            let row: Option<(String,)> = sqlx::query_as(
                "SELECT role FROM control.tenant_members \
                 WHERE tenant_id = $1 AND user_id = $2",
            )
            .bind(session.tenant_id)
            .bind(session.user_id)
            .fetch_optional(pool)
            .await?;

            match row {
                None => Err(AppError::NotAMember),
                Some((role,)) if role == "owner" || role == "admin" => Ok(()),
                Some(_) => Err(AppError::InsufficientRole),
            }
        }
    }
}

/// Readiness probe — returns 200 when the service is ready to serve traffic.
#[utoipa::path(
    get,
    path = "/ready",
    responses(
        (status = 200, description = "Service is ready", body = ProbeResponse),
        (status = 503, description = "Service is not ready")
    ),
    tag = "health"
)]
pub async fn ready_check() -> Json<ProbeResponse> {
    Json(ProbeResponse { status: "ok" })
}

/// Returns the `OpenAPI` 3.1 spec as JSON.
pub async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_status_never_received_is_stale() {
        assert_eq!(derive_status(0, i64::MAX), "stale");
    }

    #[test]
    fn derive_status_fresh_is_healthy() {
        assert_eq!(derive_status(1, 5), "healthy");
    }

    #[test]
    fn derive_status_30s_boundary_is_degraded() {
        assert_eq!(derive_status(1, 30), "degraded");
    }

    #[test]
    fn derive_status_301s_is_stale() {
        assert_eq!(derive_status(1, 301), "stale");
    }

    #[test]
    fn age_from_ms_zero_is_max() {
        assert_eq!(age_from_ms(0), i64::MAX);
    }

    #[test]
    fn age_from_ms_recent_is_small() {
        let now_ms = Utc::now().timestamp_millis();
        let age = age_from_ms(now_ms - 5000);
        assert!(age <= 6, "age should be ~5s, got {age}");
    }

    /// `/health` must return "unknown" (not "ok") when no Kafka event has
    /// ever been seen — matching the Neo4j not-configured pattern.
    #[test]
    fn kafka_liveness_never_seen_is_unknown() {
        // last_ms == 0 → age_from_ms returns i64::MAX → branch: last_ms == 0 → "unknown"
        let last_ms: i64 = 0;
        let age_secs = age_from_ms(last_ms);
        let status = if last_ms == 0 {
            "unknown"
        } else if age_secs < 300 {
            "ok"
        } else {
            "error"
        };
        assert_eq!(status, "unknown");
    }

    #[test]
    fn kafka_liveness_fresh_event_is_ok() {
        let last_ms = Utc::now().timestamp_millis() - 1000; // 1 s ago
        let age_secs = age_from_ms(last_ms);
        let status = if last_ms == 0 {
            "unknown"
        } else if age_secs < 300 {
            "ok"
        } else {
            "error"
        };
        assert_eq!(status, "ok");
    }

    #[test]
    fn kafka_liveness_stale_event_is_error() {
        let last_ms = Utc::now().timestamp_millis() - 400_000; // ~6 min ago
        let age_secs = age_from_ms(last_ms);
        let status = if last_ms == 0 {
            "unknown"
        } else if age_secs < 300 {
            "ok"
        } else {
            "error"
        };
        assert_eq!(status, "error");
    }
}
