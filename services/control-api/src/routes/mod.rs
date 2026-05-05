pub mod api_keys;
pub mod audit;
pub mod auth;
pub mod auth_logout;
pub mod auth_verify;
pub mod github;
pub mod health;
pub mod ingest;
pub mod me;
pub mod query;
pub mod repos;
pub mod tenants;

use axum::{Router, routing::{delete, get, post, put}};

use crate::routes::{
    api_keys::{create_api_key, list_api_keys, revoke_api_key},
    audit::list_audit_events,
    auth::{forgot_password, login, reset_password, signup},
    auth_logout::logout,
    auth_verify::verify_email,
    github::health::github_app_health,
    github::install::{github_callback, github_install_url},
    github::repos::list_available_repos,
    github::webhook::github_webhook,
    health::{health_check, openapi_json, ready_check},
    ingest::events_stream::events_stream,
    ingest::recent::list_recent_runs,
    ingest::stages::get_stage_timeline,
    ingest::test_publish::test_publish,
    ingest::trigger::trigger_ingestion,
    me::{get_me, switch_tenant},
    query::items::get_item,
    query::modules::get_module_tree,
    repos::{connect_repo, list_repos, trigger_ingest},
    tenants::{delete_tenant, invite_member, list_members, remove_member, transfer_ownership, update_member_role},
};
use crate::state::AppState;

pub fn build(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_check))
        .route("/ready", get(ready_check))
        .route("/openapi.json", get(openapi_json))
        .route("/v1/auth/signup", post(signup))
        .route("/v1/auth/login", post(login))
        .route("/v1/auth/logout", post(logout))
        .route("/v1/auth/verify-email", post(verify_email))
        .route("/v1/auth/forgot-password", post(forgot_password))
        .route("/v1/auth/reset-password", post(reset_password))
        .route("/v1/me", get(get_me))
        .route("/v1/me/switch-tenant", post(switch_tenant))
        .route("/v1/api-keys", post(create_api_key))
        .route("/v1/api-keys", get(list_api_keys))
        .route("/v1/api-keys/{id}", delete(revoke_api_key))
        .route("/v1/tenants/{id}", delete(delete_tenant))
        .route("/v1/tenants/{id}/members", get(list_members).post(invite_member))
        .route("/v1/tenants/{id}/members/{uid}/role", put(update_member_role))
        .route("/v1/tenants/{id}/members/{uid}", delete(remove_member))
        .route("/v1/tenants/{id}/transfer-ownership", post(transfer_ownership))
        .route("/v1/health/github-app", get(github_app_health))
        .route("/v1/github/webhook", post(github_webhook))
        .route("/v1/github/install-url", get(github_install_url))
        .route("/v1/github/callback", get(github_callback))
        .route("/v1/github/installations/{id}/available-repos", get(list_available_repos))
        .route("/v1/repos/{repo_id}/modules", get(get_module_tree))
        .route("/v1/repos", post(connect_repo).get(list_repos))
        .route("/v1/repos/{id}/ingest", post(trigger_ingest))
        .route("/v1/repos/{repo_id}/ingestions", post(trigger_ingestion))
        .route("/v1/repos/{repo_id}/items/{fqn_b64}", get(get_item))
        .route("/v1/ingestions/recent", get(list_recent_runs))
        .route("/v1/ingestions/{ingestion_run_id}/stages", get(get_stage_timeline))
        .route("/v1/ingest/events", get(events_stream))
        .route("/v1/ingest/test-publish", post(test_publish))
        .route("/v1/audit", get(list_audit_events))
        .with_state(state)
}
