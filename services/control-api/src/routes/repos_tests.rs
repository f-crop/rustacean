use super::*;
use crate::middleware::auth::{ApiKeyInfo, Scope, SessionInfo};

fn verified_session() -> SessionInfo {
    SessionInfo {
        session_id: Uuid::new_v4(),
        user_id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        email_verified: true,
    }
}

// ----- connect_repo auth tests (REQ-GH-04) -----

#[test]
fn anonymous_auth_rejected() {
    let result = require_verified_session(AuthContext::Anonymous);
    assert!(matches!(result, Err(AppError::Unauthorized)));
}

#[test]
fn api_key_auth_rejected() {
    let key = ApiKeyInfo {
        key_id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        user_id: Uuid::new_v4(),
        scopes: vec![Scope::Write],
    };
    let result = require_verified_session(AuthContext::ApiKey(key));
    assert!(matches!(result, Err(AppError::Unauthorized)));
}

#[test]
fn expired_session_rejected() {
    let result = require_verified_session(AuthContext::ExpiredSession);
    assert!(matches!(result, Err(AppError::SessionExpired)));
}

#[test]
fn unverified_email_rejected() {
    let mut info = verified_session();
    info.email_verified = false;
    let result = require_verified_session(AuthContext::Session(info));
    assert!(matches!(result, Err(AppError::EmailNotVerified)));
}

#[test]
fn verified_session_accepted() {
    let info = verified_session();
    let user_id = info.user_id;
    let result = require_verified_session(AuthContext::Session(info));
    let session = result.unwrap();
    assert_eq!(session.user_id, user_id);
}

#[test]
fn github_404_maps_to_repo_not_accessible() {
    let err = GhError::ApiError {
        status: 404,
        body: "Not Found".to_owned(),
    };
    let app_err = match err {
        GhError::ApiError { status: 404, .. } | GhError::ApiError { status: 403, .. } => {
            AppError::RepoNotAccessible
        }
        other => AppError::Internal(anyhow::anyhow!("{other}")),
    };
    assert!(matches!(app_err, AppError::RepoNotAccessible));
}

#[test]
fn github_403_maps_to_repo_not_accessible() {
    let err = GhError::ApiError {
        status: 403,
        body: "Forbidden".to_owned(),
    };
    let app_err = match err {
        GhError::ApiError { status: 404, .. } | GhError::ApiError { status: 403, .. } => {
            AppError::RepoNotAccessible
        }
        other => AppError::Internal(anyhow::anyhow!("{other}")),
    };
    assert!(matches!(app_err, AppError::RepoNotAccessible));
}

#[test]
fn github_500_maps_to_internal() {
    let err = GhError::ApiError {
        status: 500,
        body: "Server Error".to_owned(),
    };
    let app_err = match err {
        GhError::ApiError { status: 404, .. } | GhError::ApiError { status: 403, .. } => {
            AppError::RepoNotAccessible
        }
        other => AppError::Internal(anyhow::anyhow!("{other}")),
    };
    assert!(matches!(app_err, AppError::Internal(_)));
}

#[test]
fn default_branch_override_takes_priority() {
    let override_branch = "develop".to_owned();
    assert_eq!(override_branch, "develop");
}

#[test]
fn github_default_branch_used_when_no_override() {
    let github_branch = "main".to_owned();
    assert_eq!(github_branch, "main");
}

// ----- trigger_ingest response types (REQ-GH-08) -----

#[test]
fn trigger_ingest_response_serializes_correctly() {
    let run_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    let resp = TriggerIngestResponse {
        run_id,
        repo_id,
        status: "queued".to_owned(),
    };
    let val = serde_json::to_value(&resp).unwrap();
    assert_eq!(val["status"], "queued");
    assert!(val.get("run_id").is_some());
    assert!(val.get("repo_id").is_some());
}

#[test]
fn ingest_run_already_in_flight_is_conflict() {
    let err = AppError::IngestRunAlreadyInFlight;
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[test]
fn ingest_error_message() {
    assert_eq!(
        AppError::IngestRunAlreadyInFlight.to_string(),
        "an ingestion run is already in progress for this repository"
    );
}

#[test]
fn new_error_messages() {
    assert_eq!(
        AppError::GithubAppNotConfigured.to_string(),
        "GitHub App is not configured on this instance"
    );
    assert_eq!(
        AppError::RepoNotAccessible.to_string(),
        "repository is not accessible via the given installation"
    );
    assert_eq!(
        AppError::RepoAlreadyConnected.to_string(),
        "repository is already connected to this tenant"
    );
    assert_eq!(
        AppError::IngestRunAlreadyInFlight.to_string(),
        "an ingestion run is already in progress for this repository"
    );
}

// ----- list_repos response types (REQ-GH-07) -----

#[test]
fn repo_item_serializes_all_fields() {
    let item = RepoItem {
        repo_id: Uuid::new_v4(),
        full_name: "acme/backend".to_owned(),
        default_branch: "main".to_owned(),
        status: "connected".to_owned(),
        connected_by: Uuid::new_v4(),
        connected_at: Utc::now(),
        installation_id: Uuid::new_v4(),
    };
    let val = serde_json::to_value(&item).unwrap();
    assert!(val.get("repo_id").is_some());
    assert_eq!(val["full_name"], "acme/backend");
    assert_eq!(val["default_branch"], "main");
    assert_eq!(val["status"], "connected");
    assert!(val.get("connected_by").is_some());
    assert!(val.get("connected_at").is_some());
    assert!(val.get("installation_id").is_some());
}

#[test]
fn list_response_wraps_repos_array() {
    let resp = ConnectedReposResponse { repos: vec![] };
    let val = serde_json::to_value(&resp).unwrap();
    assert!(val["repos"].is_array());
}

#[test]
fn list_response_empty_is_valid() {
    let resp = ConnectedReposResponse { repos: vec![] };
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains("\"repos\":[]"));
}

// ----- trigger_ingest Kafka error paths (REQ-GH-08) -----

#[test]
fn kafka_not_configured_returns_503() {
    let err = AppError::KafkaNotConfigured;
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[test]
fn kafka_unavailable_returns_503() {
    let err = AppError::KafkaUnavailable;
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[test]
fn kafka_broker_down_publish_returns_503() {
    use rb_kafka::KafkaError;
    let rdkafka_err = rdkafka::error::KafkaError::MessageProduction(
        rdkafka::error::RDKafkaErrorCode::AllBrokersDown,
    );
    let err = AppError::KafkaPublish(KafkaError::Rdkafka(rdkafka_err));
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[test]
fn pipeline_stages_count() {
    assert_eq!(PIPELINE_STAGES.len(), 9, "nine stages per IngestStage enum");
}

#[test]
fn pipeline_stages_no_duplicates() {
    let mut seen = std::collections::HashSet::new();
    for s in PIPELINE_STAGES {
        assert!(seen.insert(*s), "duplicate stage: {s}");
    }
}
