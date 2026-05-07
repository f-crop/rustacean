//! MCP (Model Context Protocol) protocol types for Rustbrain's agent integration.
//!
//! This crate provides JSON-RPC 2.0 envelope types and MCP-specific protocol
//! structures used to implement the `POST /mcp` endpoint in control-api.
//!
//! **Architecture constraint (ADR-009 §1):** this crate has NO dependency on
//! `rb-query`.  Tool dispatch is wired inside `control-api` so the protocol
//! library stays thin and reusable.

pub mod protocol;
pub mod types;

pub use protocol::{
    ClientInfo, InitializeParams, InitializeResult, ServerCapabilities, ServerInfo,
    ToolCallParams, ToolCallResult, ToolContent, ToolDefinition, ToolsCapability,
    ToolsListResult, MCP_PROTOCOL_VERSION,
};
pub use types::{
    JsonRpcError, JsonRpcErrorResponse, JsonRpcRequest, JsonRpcResponse,
    INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR,
    TENANT_DRIFT, SESSION_NOT_FOUND, TOOL_NOT_FOUND, UNAUTHORIZED_MCP,
};
