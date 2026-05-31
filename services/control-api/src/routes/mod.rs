pub mod admin;
pub mod agents;
pub mod api_keys;
pub mod audit;
pub mod auth;
pub mod auth_logout;
pub mod auth_password_reset;
pub mod auth_verify;
pub mod github;
pub mod health;
pub mod ingest;
pub mod mcp;
pub mod me;
pub mod query;
pub mod repos;
pub mod tenants;
pub mod traces;

use axum::{
    Router,
    middleware::from_fn_with_state,
    routing::{delete, get, patch, post, put},
};

use crate::middleware::admin_auth::require_admin_token;
use crate::middleware::internal_auth::require_internal_secret;
use crate::routes::{
    admin::github::{get_app_callback, get_app_status, post_app_manifest},
    admin::partition_maintenance::partition_maintenance,
    admin::v1::{
        audit_log::list_audit_log,
        bootstrap::bootstrap_admin,
        tenants::{force_delete, impersonate, rebind_gh_install},
    },
    agents::{
        create_session, delete_session, delete_session_api_key, get_session, ingest_session_events,
        list_sessions, patch_session_status, session_events, session_events_history,
        session_log_ndjson,
    },
    api_keys::{create_api_key, list_api_keys, revoke_api_key},
    audit::list_audit_events,
    auth::{login, signup},
    auth_logout::logout,
    auth_password_reset::{forgot_password, reset_password},
    auth_verify::{resend_verification, verify_email},
    github::health::github_app_health,
    github::install::{github_callback, github_install_url},
    github::repos::list_available_repos,
    github::webhook::github_webhook,
    health::{build_info, consistency_check, health_check, openapi_json, ready_check, version},
    ingest::events_stream::events_stream,
    ingest::recent::list_recent_runs,
    ingest::stages::get_stage_timeline,
    ingest::test_publish::test_publish,
    ingest::trigger::trigger_ingestion,
    mcp::mcp_handler,
    me::{get_me, switch_tenant},
    query::graph::post_graph_query,
    query::impls::get_trait_impls,
    query::items::get_item,
    query::modules::get_module_tree,
    query::search::search,
    query::traversal::{get_callees, get_callers},
    query::usages::get_type_usages,
    repos::{connect_repo, list_repos, trigger_ingest},
    tenants::{
        delete_tenant, invite_member, list_members, remove_member, transfer_ownership,
        update_member_role,
    },
    traces::get_trace,
};
use crate::state::AppState;

#[allow(clippy::too_many_lines)]
pub fn build_public(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_check))
        .route("/health/build", get(build_info))
        .route("/ready", get(ready_check))
        .route("/openapi.json", get(openapi_json))
        .route("/v1/_version", get(version))
        .route("/v1/auth/signup", post(signup))
        .route("/v1/auth/login", post(login))
        .route("/v1/auth/logout", post(logout))
        .route("/v1/auth/verify-email", post(verify_email))
        .route("/v1/auth/resend-verification", post(resend_verification))
        .route("/v1/auth/forgot-password", post(forgot_password))
        .route("/v1/auth/reset-password", post(reset_password))
        .route("/v1/me", get(get_me))
        .route("/v1/me/switch-tenant", post(switch_tenant))
        .route("/v1/api-keys", post(create_api_key))
        .route("/v1/api-keys", get(list_api_keys))
        .route("/v1/api-keys/{id}", delete(revoke_api_key))
        .route("/v1/tenants/{id}", delete(delete_tenant))
        .route(
            "/v1/tenants/{id}/members",
            get(list_members).post(invite_member),
        )
        .route(
            "/v1/tenants/{id}/members/{uid}/role",
            put(update_member_role),
        )
        .route("/v1/tenants/{id}/members/{uid}", delete(remove_member))
        .route(
            "/v1/tenants/{id}/transfer-ownership",
            post(transfer_ownership),
        )
        .route("/v1/health/github-app", get(github_app_health))
        .route("/v1/admin/github/app-manifest", post(post_app_manifest))
        .route("/v1/admin/github/app-callback", get(get_app_callback))
        .route("/v1/admin/github/app-status", get(get_app_status))
        .route("/v1/github/webhook", post(github_webhook))
        .route("/v1/github/install-url", get(github_install_url))
        .route("/v1/github/callback", get(github_callback))
        .route(
            "/v1/github/installations/{id}/available-repos",
            get(list_available_repos),
        )
        .route("/v1/repos/{repo_id}/modules", get(get_module_tree))
        .route("/v1/repos", post(connect_repo).get(list_repos))
        .route("/v1/repos/{id}/ingest", post(trigger_ingest))
        .route("/v1/repos/{repo_id}/ingestions", post(trigger_ingestion))
        .route("/v1/repos/{repo_id}/items/{fqn_b64}", get(get_item))
        .route(
            "/v1/repos/{repo_id}/items/{fqn_b64}/impls",
            get(get_trait_impls),
        )
        .route(
            "/v1/repos/{repo_id}/items/{fqn_b64}/usages",
            get(get_type_usages),
        )
        .route("/v1/graph/query", post(post_graph_query))
        .route("/v1/search", post(search))
        .route("/v1/health/consistency", get(consistency_check))
        .route("/v1/ingestions/recent", get(list_recent_runs))
        .route(
            "/v1/ingestions/{ingestion_run_id}/stages",
            get(get_stage_timeline),
        )
        .route(
            "/v1/repos/{repo_id}/items/{fqn_b64}/callers",
            get(get_callers),
        )
        .route(
            "/v1/repos/{repo_id}/items/{fqn_b64}/callees",
            get(get_callees),
        )
        .route("/v1/ingest/events", get(events_stream))
        .route("/v1/ingest/test-publish", post(test_publish))
        .route("/v1/audit", get(list_audit_events))
        // Trace ID redirect to Grafana Tempo (ADR-012 §S4)
        .route("/v1/traces/{trace_id}", get(get_trace))
        // MCP endpoint (ADR-009)
        .route("/mcp", post(mcp_handler))
        // Admin v1 operator endpoints (ADR-012 §S1) — bearer-token gated
        .nest(
            "/api/admin/v1",
            Router::new()
                .route("/bootstrap/admin", post(bootstrap_admin))
                .route(
                    "/tenants/{tenant_id}/rebind-gh-install",
                    post(rebind_gh_install),
                )
                .route("/tenants/{tenant_id}/impersonate", post(impersonate))
                .route("/tenants/{tenant_id}/force-delete", post(force_delete))
                .route("/audit-log", get(list_audit_log))
                .route_layer(from_fn_with_state(state.clone(), require_admin_token)),
        )
        // Agent session routes (ADR-009 Option B)
        .route(
            "/v1/agents/sessions",
            post(create_session).get(list_sessions),
        )
        .route(
            "/v1/agents/sessions/{id}",
            get(get_session).delete(delete_session),
        )
        .route("/v1/agents/sessions/{id}/events", get(session_events))
        .route(
            "/v1/agents/sessions/{id}/events/history",
            get(session_events_history),
        )
        .route(
            "/v1/agents/sessions/{id}/log.ndjson",
            get(session_log_ndjson),
        )
        .with_state(state)
}

pub fn build_internal(state: AppState) -> Router {
    Router::new()
        // Internal routes for agent-runner callbacks (protected by internal secret middleware)
        .route(
            "/internal/agent/sessions/{id}/status",
            patch(patch_session_status),
        )
        .route(
            "/internal/agent/sessions/{id}/api-key",
            delete(delete_session_api_key),
        )
        .route(
            "/internal/agent/sessions/{id}/events",
            post(ingest_session_events),
        )
        // Nightly partition maintenance: seed upcoming partitions + prune expired ones.
        .route(
            "/internal/admin/partition-maintenance",
            post(partition_maintenance),
        )
        .route_layer(from_fn_with_state(state.clone(), require_internal_secret))
        .with_state(state)
}

#[deprecated(
    since = "0.1.0",
    note = "Use build_public and build_internal separately"
)]
pub fn build(state: AppState) -> Router {
    build_public(state.clone()).merge(build_internal(state))
}
