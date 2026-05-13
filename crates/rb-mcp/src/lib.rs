//! MCP (Model Context Protocol) protocol types for Rustbrain's agent integration.
//!
//! This crate provides JSON-RPC 2.0 envelope types and MCP-specific protocol
//! structures used to implement the `POST /mcp` endpoint in control-api.
//!
//! **Architecture constraint (ADR-009 §1):** this crate has NO dependency on
//! `rb-query`.  Tool dispatch is wired inside `control-api` so the protocol
//! library stays thin and reusable.

mod protocol;
mod session;
mod types;

pub use protocol::{
    ClientInfo, InitializeParams, InitializeResult, MCP_PROTOCOL_VERSION, ServerCapabilities,
    ServerInfo, ToolCallParams, ToolCallResult, ToolContent, ToolDefinition, ToolsCapability,
    ToolsListResult, phase1_tools,
};
pub use session::McpSessionStore;
pub use types::{
    INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, JsonRpcError, JsonRpcErrorResponse,
    JsonRpcRequest, JsonRpcResponse, METHOD_NOT_FOUND, PARSE_ERROR, SESSION_NOT_FOUND,
    TENANT_DRIFT, TOOL_NOT_FOUND, UNAUTHORIZED_MCP,
};
