use rb_mcp::{ToolCallResult, ToolDefinition};
use serde_json::json;

use crate::error::AppError;
use crate::state::AppState;

pub mod dispatch;

#[allow(clippy::too_many_lines)]
pub fn all_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "search_items".to_owned(),
            description: "Semantic search over code symbols in the repository graph. \
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
                          repository. Returns metadata, source location, and inline source text."
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
        ToolDefinition {
            name: "get_callers".to_owned(),
            description: "Find all functions that call the specified function. \
                          Returns a list of caller functions with their FQNs."
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
                        "description": "Fully-qualified name of the function to find callers for"
                    },
                    "depth": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 10,
                        "description": "Traversal depth (default 3)"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 200,
                        "description": "Max results to return (default 50)"
                    }
                },
                "required": ["repo_id", "fqn"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "get_callees".to_owned(),
            description: "Find all functions called by the specified function. \
                          Returns a list of callee functions with their FQNs."
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
                        "description": "Fully-qualified name of the function to find callees for"
                    },
                    "depth": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 10,
                        "description": "Traversal depth (default 3)"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 200,
                        "description": "Max results to return (default 50)"
                    }
                },
                "required": ["repo_id", "fqn"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "get_trait_impls".to_owned(),
            description: "Find all implementations of a trait. \
                          Returns both direct impls and blanket impls."
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
                        "description": "Fully-qualified name of the trait"
                    }
                },
                "required": ["repo_id", "fqn"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "run_query".to_owned(),
            description: "Execute a read-only Cypher query against the graph database. \
                          Requires read:graph scope."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Cypher query to execute"
                    },
                    "repo_id": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Optional: restrict query to this repository"
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        },
    ]
}

pub fn tools_for_scope(scope: &str) -> Vec<&'static str> {
    match scope {
        "read:items" => vec!["search_items", "get_item"],
        "read:graph" => vec!["get_callers", "get_callees", "get_trait_impls", "run_query"],
        _ => vec![],
    }
}

pub async fn dispatch_tool(
    state: &AppState,
    tenant_id: uuid::Uuid,
    tool_name: &str,
    args: &serde_json::Value,
) -> Result<ToolCallResult, AppError> {
    match tool_name {
        "search_items" => dispatch::dispatch_search_items(state, tenant_id, args).await,
        "get_item" => dispatch::dispatch_get_item(state, tenant_id, args).await,
        "get_callers" => dispatch::dispatch_get_callers(state, tenant_id, args).await,
        "get_callees" => dispatch::dispatch_get_callees(state, tenant_id, args).await,
        "get_trait_impls" => dispatch::dispatch_get_trait_impls(state, tenant_id, args).await,
        "run_query" => dispatch::dispatch_run_query(state, tenant_id, args).await,
        _ => Err(AppError::InvalidInput),
    }
}
