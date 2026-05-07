use axum::{
    extract::{Extension, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use rb_mcp::{
    InitializeParams, InitializeResult, JsonRpcErrorResponse, JsonRpcRequest, JsonRpcResponse,
    MCP_PROTOCOL_VERSION, METHOD_NOT_FOUND, SESSION_NOT_FOUND, TENANT_DRIFT,
    TOOL_NOT_FOUND, ToolCallParams, ToolsListResult, UNAUTHORIZED_MCP, phase2_tools,
};
use uuid::Uuid;

use crate::{
    middleware::auth::AuthContext,
    state::AppState,
    tools::{all_tools, dispatch_tool, tools_for_scope},
};

pub async fn mcp_post_handler(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    dispatch(&state, &auth, headers, body).await.unwrap_or_else(|e| e)
}

type McpResult = Result<Response, Response>;

async fn dispatch(
    state: &AppState,
    auth: &AuthContext,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> McpResult {
    let rpc: JsonRpcRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("MCP parse error: {}", e);
            return Err(rpc_err(None, rb_mcp::PARSE_ERROR, "parse error"));
        }
    };

    if rpc.jsonrpc != "2.0" {
        return Err(rpc_err(rpc.id, rb_mcp::INVALID_REQUEST, "jsonrpc must be '2.0'"));
    }

    match rpc.method.as_str() {
        "initialize" => handle_initialize(state, auth, rpc),
        "notifications/initialized" => {
            Ok((StatusCode::ACCEPTED, "").into_response())
        }
        "ping" => Ok(rpc_ok(rpc.id, serde_json::json!({}))),
        "tools/list" => handle_tools_list(state, auth, &headers, rpc),
        "tools/call" => handle_tools_call(state, auth, headers, rpc).await,
        _ => Err(rpc_err(rpc.id, METHOD_NOT_FOUND, "method not found")),
    }
}

fn handle_initialize(
    state: &AppState,
    auth: &AuthContext,
    rpc: JsonRpcRequest,
) -> McpResult {
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
                    "MCP client requested newer protocol; proceeding with {}"
                    , MCP_PROTOCOL_VERSION
                );
            }
        }
    }

    let session_id = state.mcp_sessions.create(tenant_id);
    tracing::info!(tenant_id = %tenant_id, session_id = %session_id, "MCP session created");

    let result = serde_json::to_value(InitializeResult::new()).unwrap_or(serde_json::json!({}));
    let response = rpc_ok(rpc.id, result);
    
    Ok(response)
}

fn handle_tools_list(
    _state: &AppState,
    _auth: &AuthContext,
    _headers: &HeaderMap,
    rpc: JsonRpcRequest,
) -> McpResult {
    let result = serde_json::to_value(ToolsListResult { tools: all_tools() })
        .unwrap_or(serde_json::json!({}));
    Ok(rpc_ok(rpc.id, result))
}

async fn handle_tools_call(
    state: &AppState,
    auth: &AuthContext,
    headers: HeaderMap,
    rpc: JsonRpcRequest,
) -> McpResult {
    let session_tenant_id = validate_session(state, auth, &headers, rpc.id.clone())?;

    let params: ToolCallParams = match rpc.params.as_ref().and_then(|p| {
        serde_json::from_value(p.clone()).ok()
    }) {
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
    
    let required_scopes: Vec<_> = tools_for_scope("read:items")
        .into_iter()
        .chain(tools_for_scope("read:graph"))
        .collect();
    
    if !required_scopes.contains(&params.name.as_str()) {
        return Err(rpc_err(rpc.id, TOOL_NOT_FOUND, "unknown tool"));
    }
    
    let has_read_items = auth.has_scope("read:items");
    let has_read_graph = auth.has_scope("read:graph");
    
    let items_tools = tools_for_scope("read:items");
    let graph_tools = tools_for_scope("read:graph");
    
    let allowed = if items_tools.contains(&params.name.as_str()) {
        has_read_items
    } else if graph_tools.contains(&params.name.as_str()) {
        has_read_graph
    } else {
        false
    };
    
    if !allowed {
        return Err(rpc_err(rpc.id, UNAUTHORIZED_MCP, "insufficient scope"));
    }

    let tool_result = dispatch_tool(state, session_tenant_id, &params.name, &args).await;

    let call_result = match tool_result {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                tool = %params.name,
                tenant_id = %session_tenant_id,
                "MCP tool call failed: {:?}", e
            );
            rb_mcp::ToolCallResult::error(format!("{:?}", e))
        }
    };

    let result = serde_json::to_value(&call_result).unwrap_or(serde_json::json!({}));
    Ok(rpc_ok(rpc.id, result))
}

fn require_auth_tenant(auth: &AuthContext) -> Result<Uuid, ()> {
    match auth {
        AuthContext::ApiKey(info) if info.scopes.contains(&"read:items".to_owned()) || 
                                     info.scopes.contains(&"read:graph".to_owned()) => {
            Ok(info.tenant_id)
        }
        _ => Err(()),
    }
}

fn validate_session(
    state: &AppState,
    auth: &AuthContext,
    headers: &HeaderMap,
    req_id: Option<serde_json::Value>,
) -> Result<Uuid, Response> {
    let auth_tenant_id = match auth {
        AuthContext::ApiKey(info) if info.scopes.contains(&"read:items".to_owned()) ||
                                     info.scopes.contains(&"read:graph".to_owned()) => {
            info.tenant_id
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
        None => Err(rpc_err(req_id, SESSION_NOT_FOUND, "session not found or expired")),
        Some(session_tenant) if session_tenant != auth_tenant_id => {
            tracing::warn!(
                auth_tenant = %auth_tenant_id,
                session_tenant = %session_tenant,
                "MCP tenant drift detected"
            );
            Err(rpc_err(req_id, TENANT_DRIFT, "tenant mismatch"))
        }
        Some(session_tenant) => Ok(session_tenant),
    }
}

fn rpc_ok(id: Option<serde_json::Value>, result: serde_json::Value) -> Response {
    axum::Json(JsonRpcResponse::ok(id, result)).into_response()
}

fn rpc_err(id: Option<serde_json::Value>, code: i32, message: &str) -> Response {
    axum::Json(JsonRpcErrorResponse::new(id, code, message)).into_response()
}
