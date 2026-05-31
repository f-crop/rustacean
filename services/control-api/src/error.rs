use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use rb_kafka::KafkaError;
use serde_json::json;
use thiserror::Error;

/// Top-level application error type.
///
/// Every variant maps to an HTTP status code, a stable machine-readable
/// `error` string, and a human-readable message.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AppError {
    #[error("not found")]
    NotFound,
    #[error("email already registered")]
    EmailTaken,
    #[error("password must be at least 12 characters")]
    WeakPassword,
    #[error("invalid email address")]
    InvalidEmail,
    #[error("invalid request")]
    InvalidInput,
    #[error("invalid or expired token")]
    InvalidToken,
    #[error("authentication required")]
    Unauthorized,
    #[error("insufficient role for this operation")]
    InsufficientRole,
    #[error("api key lacks required scope")]
    InsufficientScope,
    #[error("cannot remove or demote the tenant owner")]
    CannotRemoveOwner,
    #[error("user is not a member of this tenant")]
    NotAMember,
    #[error("user is already a member of this tenant")]
    AlreadyMember,
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("account suspended")]
    AccountSuspended,
    #[error("session expired")]
    SessionExpired,
    #[error("email address not yet verified")]
    EmailNotVerified,
    #[error("GitHub App is not configured on this instance")]
    GithubAppNotConfigured,
    #[error("this GitHub installation is already linked to a different tenant")]
    GithubInstallationConflict,
    #[error("repository is not accessible via the given installation")]
    RepoNotAccessible,
    #[error("installation belongs to a different GitHub App; reinstall the active App to continue")]
    InstallationForDifferentApp { install_url: String },
    #[error("repository is already connected to this tenant")]
    RepoAlreadyConnected,
    #[error("an ingestion run is already in progress for this repository")]
    IngestRunAlreadyInFlight,
    #[error("X-Confirm header must match the tenant slug exactly")]
    ConfirmationMismatch,
    #[error("Neo4j graph is not configured on this instance")]
    GraphUnavailable,
    #[error("required upstream service is not available")]
    ServiceUnavailable,
    #[error("Kafka producer is not configured on this instance")]
    KafkaNotConfigured,
    #[error("Neo4j graph store is not configured on this instance")]
    GraphNotConfigured,
    #[error("query contains Cypher write operators but read_only is true")]
    CypherWriteDenied,
    #[error("graph query error: {0}")]
    CypherQuery(#[from] rb_storage_neo4j::CypherError),
    #[error("Kafka brokers are not reachable")]
    KafkaUnavailable,
    #[error("failed to publish ingestion event to Kafka: {0}")]
    KafkaPublish(#[from] KafkaError),
    #[error("process session cap reached; try again later")]
    SessionCapExceeded,
    #[error("session creation rate limit exceeded; retry after {retry_after_secs}s")]
    SessionRateLimitExceeded { retry_after_secs: u64 },
    #[error("tenant active session cap exceeded")]
    TenantSessionCapExceeded,
    #[error("runtime adapter is not configured on this instance")]
    RuntimeNotConfigured,
    #[error("session is not currently running")]
    SessionNotRunning,
    #[error("redirect_uri origin does not match the allowed origin")]
    BadRedirectUri,
    #[error("admin user already exists; bootstrap is a one-time operation")]
    AdminBootstrapConflict,
    #[error("tenant row counts shifted between phase-1 and phase-2; re-run from dry-run")]
    AdminForceDeleteConflict,
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("auth error: {0}")]
    Auth(#[from] rb_auth::AuthError),
    #[error("email error: {0}")]
    Email(#[from] rb_email::EmailError),
    #[error("internal server error")]
    Internal(#[from] anyhow::Error),
    #[error("query error: {0}")]
    Query(#[from] rb_query::QueryError),
}

impl IntoResponse for AppError {
    #[allow(clippy::too_many_lines)]
    fn into_response(self) -> Response {
        // `retry_after_secs` is set on 429 variants that know how long the
        // client should wait — it becomes the `Retry-After` response header
        // per RFC 6585 §4.
        let mut retry_after_secs: Option<u64> = None;
        let (status, code, message) = match &self {
            AppError::NotFound => (StatusCode::NOT_FOUND, "not_found", self.to_string()),
            AppError::EmailTaken => (StatusCode::CONFLICT, "email_taken", self.to_string()),
            AppError::WeakPassword => (StatusCode::BAD_REQUEST, "weak_password", self.to_string()),
            AppError::InvalidEmail => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_email",
                self.to_string(),
            ),
            AppError::InvalidInput => (StatusCode::BAD_REQUEST, "invalid_input", self.to_string()),
            AppError::InvalidToken => (StatusCode::BAD_REQUEST, "invalid_token", self.to_string()),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized", self.to_string()),
            AppError::InsufficientRole => {
                (StatusCode::FORBIDDEN, "insufficient_role", self.to_string())
            }
            AppError::InsufficientScope => (
                StatusCode::FORBIDDEN,
                "insufficient_scope",
                self.to_string(),
            ),
            AppError::CannotRemoveOwner => (
                StatusCode::BAD_REQUEST,
                "cannot_remove_owner",
                self.to_string(),
            ),
            AppError::NotAMember => (StatusCode::FORBIDDEN, "not_a_member", self.to_string()),
            AppError::AlreadyMember => (StatusCode::CONFLICT, "already_member", self.to_string()),
            AppError::InvalidCredentials => (
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
                self.to_string(),
            ),
            AppError::AccountSuspended => {
                (StatusCode::FORBIDDEN, "account_suspended", self.to_string())
            }
            AppError::SessionExpired => (
                StatusCode::UNAUTHORIZED,
                "session_expired",
                self.to_string(),
            ),
            AppError::EmailNotVerified => (
                StatusCode::FORBIDDEN,
                "email_not_verified",
                self.to_string(),
            ),
            AppError::GithubAppNotConfigured => (
                StatusCode::SERVICE_UNAVAILABLE,
                "github_app_not_configured",
                self.to_string(),
            ),
            AppError::GithubInstallationConflict => (
                StatusCode::CONFLICT,
                "github_installation_conflict",
                self.to_string(),
            ),
            AppError::RepoNotAccessible => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "repo_not_accessible",
                self.to_string(),
            ),
            AppError::InstallationForDifferentApp { install_url } => {
                let body = Json(serde_json::json!({
                    "error": "installation_for_different_app",
                    "message": self.to_string(),
                    "install_url": install_url,
                }));
                return (StatusCode::CONFLICT, body).into_response();
            }
            AppError::RepoAlreadyConnected => (
                StatusCode::CONFLICT,
                "repo_already_connected",
                self.to_string(),
            ),
            AppError::IngestRunAlreadyInFlight => (
                StatusCode::CONFLICT,
                "ingest_run_already_in_flight",
                self.to_string(),
            ),
            AppError::ConfirmationMismatch => (
                StatusCode::BAD_REQUEST,
                "confirmation_mismatch",
                self.to_string(),
            ),
            AppError::GraphUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "graph_unavailable",
                self.to_string(),
            ),
            AppError::ServiceUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                self.to_string(),
            ),
            AppError::KafkaNotConfigured => (
                StatusCode::SERVICE_UNAVAILABLE,
                "kafka_not_configured",
                self.to_string(),
            ),
            AppError::GraphNotConfigured => (
                StatusCode::SERVICE_UNAVAILABLE,
                "graph_not_configured",
                self.to_string(),
            ),
            AppError::CypherWriteDenied => (
                StatusCode::BAD_REQUEST,
                "cypher_write_denied",
                self.to_string(),
            ),
            AppError::CypherQuery(e) => {
                tracing::error!(error = %e, "cypher query error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "graph_query_error",
                    "graph query failed".to_owned(),
                )
            }
            AppError::KafkaUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "kafka_unavailable",
                self.to_string(),
            ),
            AppError::KafkaPublish(e) if e.is_broker_unavailable() => (
                StatusCode::SERVICE_UNAVAILABLE,
                "kafka_unavailable",
                "Kafka broker is not available; try again later".to_owned(),
            ),
            AppError::KafkaPublish(e) => {
                tracing::error!(error = %e, "kafka publish error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "kafka_publish_error",
                    "failed to queue ingestion event".to_owned(),
                )
            }
            AppError::SessionCapExceeded => (
                StatusCode::TOO_MANY_REQUESTS,
                "session_cap_exceeded",
                self.to_string(),
            ),
            AppError::SessionRateLimitExceeded {
                retry_after_secs: secs,
            } => {
                retry_after_secs = Some(*secs);
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    "rate_limit_exceeded",
                    format!("session creation rate limit exceeded; retry after {secs}s"),
                )
            }
            AppError::TenantSessionCapExceeded => (
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limit_exceeded",
                "tenant active session cap exceeded".to_owned(),
            ),
            AppError::RuntimeNotConfigured => (
                StatusCode::SERVICE_UNAVAILABLE,
                "runtime_not_configured",
                self.to_string(),
            ),
            AppError::SessionNotRunning => (
                StatusCode::CONFLICT,
                "session_not_running",
                "session is not currently running".to_owned(),
            ),
            AppError::BadRedirectUri => (
                StatusCode::BAD_REQUEST,
                "bad_redirect_uri",
                self.to_string(),
            ),
            AppError::AdminBootstrapConflict => (
                StatusCode::CONFLICT,
                "admin_bootstrap_conflict",
                self.to_string(),
            ),
            AppError::AdminForceDeleteConflict => (
                StatusCode::CONFLICT,
                "force_delete_conflict",
                self.to_string(),
            ),
            AppError::Database(e) => {
                tracing::error!(error = %e, "database error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "internal server error".to_owned(),
                )
            }
            AppError::Auth(rb_auth::AuthError::RateLimited {
                retry_after_secs: secs,
            }) => {
                retry_after_secs = Some(*secs);
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    "rate_limited",
                    format!("too many requests, retry after {secs}s"),
                )
            }
            AppError::Auth(e) => {
                tracing::error!(error = %e, "auth error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "internal server error".to_owned(),
                )
            }
            AppError::Email(e) => {
                tracing::warn!(error = %e, "email delivery error");
                // Non-fatal — signup succeeds even if email fails
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "internal server error".to_owned(),
                )
            }
            AppError::Internal(e) => {
                tracing::error!(error = %e, "unhandled internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "internal server error".to_owned(),
                )
            }
            AppError::Query(e) => {
                tracing::error!(error = %e, "query error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "internal server error".to_owned(),
                )
            }
        };
        let mut response =
            (status, Json(json!({ "error": code, "message": message }))).into_response();
        if let Some(secs) = retry_after_secs {
            // `HeaderValue::from(u64)` cannot fail; an integer-form `Retry-After`
            // is always a valid header value (RFC 6585 §4, RFC 7231 §7.1.3).
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(secs));
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_rate_limit_sets_retry_after_header() {
        let resp = AppError::SessionRateLimitExceeded {
            retry_after_secs: 42,
        }
        .into_response();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            resp.headers()
                .get(header::RETRY_AFTER)
                .map(|v| v.to_str().unwrap()),
            Some("42")
        );
    }

    #[test]
    fn auth_rate_limited_sets_retry_after_header() {
        let resp = AppError::Auth(rb_auth::AuthError::RateLimited {
            retry_after_secs: 7,
        })
        .into_response();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            resp.headers()
                .get(header::RETRY_AFTER)
                .map(|v| v.to_str().unwrap()),
            Some("7")
        );
    }

    #[test]
    fn non_rate_limit_responses_omit_retry_after() {
        let resp = AppError::NotFound.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert!(resp.headers().get(header::RETRY_AFTER).is_none());
    }

    #[test]
    fn session_cap_exceeded_omits_retry_after() {
        // SessionCapExceeded is a 429 but has no known retry window — header
        // intentionally omitted (clients should fall back to a default backoff).
        let resp = AppError::SessionCapExceeded.into_response();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(resp.headers().get(header::RETRY_AFTER).is_none());
    }
}
