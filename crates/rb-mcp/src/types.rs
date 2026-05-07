//! JSON-RPC 2.0 envelope types.
//!
//! Spec: <https://www.jsonrpc.org/specification>

use serde::{Deserialize, Serialize};

/// Expected value of the `jsonrpc` field in every message.
pub const JSONRPC_VERSION: &str = "2.0";

// ---------------------------------------------------------------------------
// Error codes
// ---------------------------------------------------------------------------

/// Standard JSON-RPC 2.0 parse error (invalid JSON received).
pub const PARSE_ERROR: i32 = -32_700;
/// Invalid JSON-RPC request object.
pub const INVALID_REQUEST: i32 = -32_600;
/// Method does not exist or is not available.
pub const METHOD_NOT_FOUND: i32 = -32_601;
/// Invalid method parameters.
pub const INVALID_PARAMS: i32 = -32_602;
/// Internal JSON-RPC error.
pub const INTERNAL_ERROR: i32 = -32_603;

/// Auth context at MCP session does not match the current request tenant.
pub const TENANT_DRIFT: i32 = -32_000;
/// `Mcp-Session-Id` header references an unknown or expired session.
pub const SESSION_NOT_FOUND: i32 = -32_001;
/// The requested tool name is not registered on this server.
pub const TOOL_NOT_FOUND: i32 = -32_002;
/// Request lacks valid authentication credentials.
pub const UNAUTHORIZED_MCP: i32 = -32_003;

// ---------------------------------------------------------------------------
// Request / response envelopes
// ---------------------------------------------------------------------------

/// JSON-RPC 2.0 inbound request (or notification when `id` is absent).
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    /// Must equal `"2.0"`.
    pub jsonrpc: String,
    /// Absent for notifications (e.g. `notifications/initialized`).
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    pub method: String,
    pub params: Option<serde_json::Value>,
}

/// JSON-RPC 2.0 success response.
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: Option<serde_json::Value>,
    pub result: serde_json::Value,
}

impl JsonRpcResponse {
    pub fn ok(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        Self { jsonrpc: JSONRPC_VERSION, id, result }
    }
}

/// JSON-RPC 2.0 error response.
#[derive(Debug, Serialize)]
pub struct JsonRpcErrorResponse {
    pub jsonrpc: &'static str,
    pub id: Option<serde_json::Value>,
    pub error: JsonRpcError,
}

impl JsonRpcErrorResponse {
    pub fn new(id: Option<serde_json::Value>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            error: JsonRpcError { code, message: message.into(), data: None },
        }
    }
}

/// Error detail nested inside a [`JsonRpcErrorResponse`].
#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ok_response_serialises_correctly() {
        let resp = JsonRpcResponse::ok(Some(json!(1)), json!({"answer": 42}));
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 1);
        assert_eq!(v["result"]["answer"], 42);
        assert!(v.get("error").is_none());
    }

    #[test]
    fn error_response_omits_data_when_none() {
        let resp = JsonRpcErrorResponse::new(Some(json!(2)), METHOD_NOT_FOUND, "not found");
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["error"]["code"], METHOD_NOT_FOUND);
        assert!(v["error"].get("data").is_none());
    }

    #[test]
    fn notification_has_null_id() {
        let rpc: JsonRpcRequest = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
        .unwrap();
        assert!(rpc.id.is_none());
    }
}
