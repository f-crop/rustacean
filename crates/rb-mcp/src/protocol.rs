//! MCP 1.0 protocol types: initialize, tools/list, tools/call, ping.
//!
//! Reference: <https://spec.modelcontextprotocol.io/specification/>
//!
//! Rustbrain Phase 1 supports two read-only tools: `search_items` and
//! `get_item`.  Phase 2 adds `get_callers`, `get_callees`, `get_trait_impls`,
//! and `run_query` (admin-only).

use serde::{Deserialize, Serialize};
use serde_json::json;

/// MCP protocol version spoken by this server.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

// ---------------------------------------------------------------------------
// initialize
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    #[serde(rename = "clientInfo")]
    pub client_info: Option<ClientInfo>,
}

#[derive(Debug, Deserialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: &'static str,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
    pub capabilities: ServerCapabilities,
}

impl InitializeResult {
    pub fn new() -> Self {
        Self {
            protocol_version: MCP_PROTOCOL_VERSION,
            server_info: ServerInfo {
                name: "rustbrain".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
            capabilities: ServerCapabilities {
                tools: ToolsCapability { list_changed: false },
            },
        }
    }
}

impl Default for InitializeResult {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Serialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Serialize)]
pub struct ServerCapabilities {
    pub tools: ToolsCapability,
}

#[derive(Debug, Serialize)]
pub struct ToolsCapability {
    /// Whether the tool list may change during a session.
    /// False for Phase 1 — the tool set is static.
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

// ---------------------------------------------------------------------------
// tools/list
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ToolsListResult {
    pub tools: Vec<ToolDefinition>,
}

#[derive(Debug, Serialize, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

/// Returns the Phase 1 tool catalogue (search_items + get_item).
pub fn phase1_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "search_items".to_owned(),
            description: "Semantic search over code symbols in the Rustbrain repository graph. \
                          Returns ranked fully-qualified names matching the natural-language query."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Natural-language or code search query"
                    },
                    "repo_id": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Optional: restrict search to this repository UUID"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 50,
                        "description": "Max results to return (default 10, max 50)"
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "get_item".to_owned(),
            description: "Fetch a code symbol by its fully-qualified name (FQN) within a \
                          repository. Returns metadata, source location, and inline source text \
                          for items ≤ 512 KiB."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "repo_id": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Repository UUID"
                    },
                    "fqn": {
                        "type": "string",
                        "description": "Fully-qualified name of the code symbol (e.g. my_crate::module::MyStruct)"
                    }
                },
                "required": ["repo_id", "fqn"],
                "additionalProperties": false
            }),
        },
    ]
}

// ---------------------------------------------------------------------------
// tools/call
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ToolCallParams {
    pub name: String,
    /// Tool-specific arguments matching the tool's `inputSchema`.
    pub arguments: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct ToolCallResult {
    pub content: Vec<ToolContent>,
    /// Present and `true` only when the tool execution produced an error.
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

impl ToolCallResult {
    pub fn success(text: impl Into<String>) -> Self {
        Self { content: vec![ToolContent::text(text)], is_error: None }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self { content: vec![ToolContent::text(text)], is_error: Some(true) }
    }
}

#[derive(Debug, Serialize)]
pub struct ToolContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
}

impl ToolContent {
    pub fn text(s: impl Into<String>) -> Self {
        Self { content_type: "text".to_owned(), text: s.into() }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase1_tools_returns_two_entries() {
        let tools = phase1_tools();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "search_items");
        assert_eq!(tools[1].name, "get_item");
    }

    #[test]
    fn initialize_result_uses_protocol_version() {
        let result = InitializeResult::new();
        assert_eq!(result.protocol_version, MCP_PROTOCOL_VERSION);
        assert_eq!(result.server_info.name, "rustbrain");
    }

    #[test]
    fn tool_call_result_error_flag() {
        let r = ToolCallResult::error("oops");
        assert_eq!(r.is_error, Some(true));
        let s = ToolCallResult::success("ok");
        assert!(s.is_error.is_none());
    }
}
