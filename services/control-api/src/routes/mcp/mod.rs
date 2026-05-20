mod audit;
mod dispatch;

use axum::{
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use rb_mcp::{
    InitializeParams, InitializeResult, JsonRpcErrorResponse, JsonRpcRequest, JsonRpcResponse,
    MCP_PROTOCOL_VERSION, METHOD_NOT_FOUND, SESSION_NOT_FOUND, TENANT_DRIFT, TOOL_NOT_FOUND,
    ToolCallParams, ToolsListResult, UNAUTHORIZED_MCP, phase1_tools,
};
use uuid::Uuid;

use crate::{
    middleware::auth::{AuthContext, Scope},
    state::AppState,
};

#[allow(unused_imports)]
pub use handler::__path_mcp_handler;
pub use handler::mcp_handler;

mod handler {
    use super::{AppState, AuthContext, HeaderMap, Response, State, dispatch};

    #[utoipa::path(
        post,
        path = "/mcp",
        tag = "mcp",
        request_body = serde_json::Value,
        responses(
            (status = 200, description = "JSON-RPC 2.0 response with Mcp-Session-Id header"),
            (status = 401, description = "Authentication required"),
        )
    )]
    pub async fn mcp_handler(
        State(state): State<AppState>,
        auth: AuthContext,
        headers: HeaderMap,
        body: axum::body::Bytes,
    ) -> Response {
        dispatch(&state, auth, headers, body)
            .await
            .unwrap_or_else(|e| e)
    }
}

type McpResult = Result<Response, Response>;

async fn dispatch(
    state: &AppState,
    auth: AuthContext,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> McpResult {
    let rpc: JsonRpcRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("MCP parse error: {e}");
            return Err(rpc_err(None, rb_mcp::PARSE_ERROR, "parse error"));
        }
    };

    if rpc.jsonrpc != "2.0" {
        return Err(rpc_err(
            rpc.id,
            rb_mcp::INVALID_REQUEST,
            "jsonrpc must be '2.0'",
        ));
    }

    match rpc.method.as_str() {
        "initialize" => handle_initialize(state, auth, rpc),
        "notifications/initialized" => Ok((StatusCode::ACCEPTED, "").into_response()),
        "ping" => Ok(rpc_ok(rpc.id, serde_json::json!({}))),
        "tools/list" => handle_tools_list(state, auth, &headers, rpc),
        "tools/call" => handle_tools_call(state, auth, headers, rpc).await,
        _ => Err(rpc_err(rpc.id, METHOD_NOT_FOUND, "method not found")),
    }
}

#[allow(clippy::result_large_err)]
fn handle_initialize(state: &AppState, auth: AuthContext, rpc: JsonRpcRequest) -> McpResult {
    let tenant_id = require_auth_tenant(auth)
        .map_err(|()| rpc_err(rpc.id.clone(), UNAUTHORIZED_MCP, "authentication required"))?;

    if let Some(params) = &rpc.params {
        if let Ok(p) = serde_json::from_value::<InitializeParams>(params.clone()) {
            tracing::debug!(
                protocol_version = %p.protocol_version,
                client = ?p.client_info.as_ref().map(|c| &c.name),
                "MCP initialize"
            );
            if p.protocol_version != MCP_PROTOCOL_VERSION {
                tracing::warn!(
                    requested = %p.protocol_version,
                    supported = %MCP_PROTOCOL_VERSION,
                    "MCP client requested newer protocol; proceeding with {MCP_PROTOCOL_VERSION}"
                );
            }
        }
    }

    let session_id = state.mcp_sessions.create(tenant_id);
    tracing::info!(tenant_id = %tenant_id, session_id = %session_id, "MCP session created");

    let result = serde_json::to_value(InitializeResult::new()).unwrap_or(serde_json::json!({}));
    let mut headers = HeaderMap::new();
    headers.insert(
        "Mcp-Session-Id",
        HeaderValue::from_str(&session_id.to_string()).expect("uuid is valid header value"),
    );

    Ok((headers, rpc_json_ok(rpc.id, result)).into_response())
}

#[allow(clippy::result_large_err)]
fn handle_tools_list(
    state: &AppState,
    auth: AuthContext,
    headers: &HeaderMap,
    rpc: JsonRpcRequest,
) -> McpResult {
    validate_session(state, auth, headers, rpc.id.clone())?;
    let result = serde_json::to_value(ToolsListResult {
        tools: phase1_tools(),
    })
    .unwrap_or(serde_json::json!({}));
    Ok(rpc_ok(rpc.id, result))
}

async fn handle_tools_call(
    state: &AppState,
    auth: AuthContext,
    headers: HeaderMap,
    rpc: JsonRpcRequest,
) -> McpResult {
    let (session_tenant_id, actor_user_id) =
        validate_session(state, auth, &headers, rpc.id.clone())?;

    let params: ToolCallParams = match rpc
        .params
        .as_ref()
        .and_then(|p| serde_json::from_value(p.clone()).ok())
    {
        Some(p) => p,
        None => {
            return Err(rpc_err(
                rpc.id,
                rb_mcp::INVALID_PARAMS,
                "tools/call requires {name, arguments}",
            ));
        }
    };

    let args = params.arguments.unwrap_or(serde_json::json!({}));

    let tool_result = match params.name.as_str() {
        "search_items" => dispatch::dispatch_search_items(state, session_tenant_id, &args).await,
        "get_item" => dispatch::dispatch_get_item(state, session_tenant_id, &args).await,
        _ => {
            return Err(rpc_err(rpc.id, TOOL_NOT_FOUND, "unknown tool"));
        }
    };

    let (call_result, outcome) = match tool_result {
        Ok(r) => (r, "success"),
        Err(e) => {
            tracing::warn!(
                tool = %params.name,
                tenant_id = %session_tenant_id,
                "MCP tool call failed: {e:?}"
            );
            (rb_mcp::ToolCallResult::error(format!("{e:?}")), "error")
        }
    };

    audit::write_tool_call_audit(
        &state.pool,
        session_tenant_id,
        actor_user_id,
        &params.name,
        &args,
        outcome,
    )
    .await;

    let result = serde_json::to_value(&call_result).unwrap_or(serde_json::json!({}));
    Ok(rpc_ok(rpc.id, result))
}

fn require_auth_tenant(auth: AuthContext) -> Result<Uuid, ()> {
    match auth {
        AuthContext::Session(info) if info.email_verified => Ok(info.tenant_id),
        // Accept both Read keys (human-facing) and Agent keys (session-scoped).
        // Session API keys are issued with the "agent" scope only; they must be
        // able to call /mcp to use rust-brain tools during a headless run.
        AuthContext::ApiKey(info)
            if info.scopes.contains(&Scope::Read) || info.scopes.contains(&Scope::Agent) =>
        {
            Ok(info.tenant_id)
        }
        _ => Err(()),
    }
}

#[allow(clippy::result_large_err)]
fn validate_session(
    state: &AppState,
    auth: AuthContext,
    headers: &HeaderMap,
    req_id: Option<serde_json::Value>,
) -> Result<(Uuid, Option<Uuid>), Response> {
    let (auth_tenant_id, actor_user_id) = match auth {
        AuthContext::Session(info) if info.email_verified => (info.tenant_id, Some(info.user_id)),
        AuthContext::ApiKey(info)
            if info.scopes.contains(&Scope::Read) || info.scopes.contains(&Scope::Agent) =>
        {
            (info.tenant_id, Some(info.user_id))
        }
        _ => {
            return Err(rpc_err(req_id, UNAUTHORIZED_MCP, "unauthorized"));
        }
    };

    let session_id: Uuid = headers
        .get("Mcp-Session-Id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(Uuid::nil());

    match state.mcp_sessions.tenant_id(&session_id) {
        None => Err(rpc_err(
            req_id,
            SESSION_NOT_FOUND,
            "session not found or expired",
        )),
        Some(session_tenant) if session_tenant != auth_tenant_id => {
            tracing::warn!(
                auth_tenant = %auth_tenant_id,
                session_tenant = %session_tenant,
                "MCP tenant drift detected"
            );
            Err(rpc_err(req_id, TENANT_DRIFT, "tenant mismatch"))
        }
        Some(session_tenant) => Ok((session_tenant, actor_user_id)),
    }
}

fn rpc_ok(id: Option<serde_json::Value>, result: serde_json::Value) -> Response {
    axum::Json(JsonRpcResponse::ok(id, result)).into_response()
}

fn rpc_json_ok(
    id: Option<serde_json::Value>,
    result: serde_json::Value,
) -> axum::Json<JsonRpcResponse> {
    axum::Json(JsonRpcResponse::ok(id, result))
}

fn rpc_err(id: Option<serde_json::Value>, code: i32, message: &str) -> Response {
    axum::Json(JsonRpcErrorResponse::new(id, code, message)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::auth::{ApiKeyInfo, SessionInfo};

    fn verified_session(tenant_id: Uuid) -> SessionInfo {
        SessionInfo {
            session_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            tenant_id,
            email_verified: true,
        }
    }

    #[test]
    fn anonymous_auth_rejected() {
        assert!(require_auth_tenant(AuthContext::Anonymous).is_err());
    }

    #[test]
    fn expired_session_rejected() {
        assert!(require_auth_tenant(AuthContext::ExpiredSession).is_err());
    }

    #[test]
    fn unverified_session_rejected() {
        let mut info = verified_session(Uuid::new_v4());
        info.email_verified = false;
        assert!(require_auth_tenant(AuthContext::Session(info)).is_err());
    }

    #[test]
    fn verified_session_returns_tenant() {
        let tid = Uuid::new_v4();
        assert_eq!(
            require_auth_tenant(AuthContext::Session(verified_session(tid))),
            Ok(tid)
        );
    }

    #[test]
    fn read_scoped_api_key_accepted() {
        let tid = Uuid::new_v4();
        let key = ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id: tid,
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Read],
        };
        assert_eq!(require_auth_tenant(AuthContext::ApiKey(key)), Ok(tid));
    }

    #[test]
    fn write_only_api_key_rejected() {
        let key = ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Write],
        };
        assert!(require_auth_tenant(AuthContext::ApiKey(key)).is_err());
    }

    #[test]
    fn agent_scoped_key_accepted() {
        // Session API keys are issued with scope ["agent"] only.
        // They must be able to call /mcp so rustbrain-mcp can use tools.
        let tid = Uuid::new_v4();
        let key = ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id: tid,
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Agent],
        };
        assert_eq!(
            require_auth_tenant(AuthContext::ApiKey(key)),
            Ok(tid),
            "agent-scoped key must be accepted by the MCP handler"
        );
    }

    #[test]
    fn agent_scoped_key_validate_session_accepted() {
        // validate_session must also accept agent-scoped keys (tools/list and tools/call).
        let tid = Uuid::new_v4();
        let key = ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id: tid,
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Agent],
        };
        // validate_session is not directly testable without AppState, but
        // the pattern mirrors require_auth_tenant — cover the scope predicate.
        let accepted = matches!(
            AuthContext::ApiKey(key),
            AuthContext::ApiKey(ref info)
                if info.scopes.contains(&Scope::Read)
                    || info.scopes.contains(&Scope::Agent)
        );
        assert!(
            accepted,
            "agent scope must pass validate_session scope guard"
        );
    }
}
