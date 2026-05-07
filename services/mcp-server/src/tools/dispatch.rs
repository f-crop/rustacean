use rb_mcp::ToolCallResult;
use rb_query::{
    TraversalOptions, fetch_callers, fetch_callees, fetch_trait_impls, items, semantic_search,
    SearchOptions, DEFAULT_SEARCH_LIMIT, MAX_SEARCH_LIMIT,
};
use rb_schemas::TenantId;
use rb_tenant::TenantCtx;
use uuid::Uuid;

use crate::state::AppState;
use crate::error::AppError;

#[allow(clippy::cast_possible_truncation)]
pub async fn dispatch_search_items(
    state: &AppState,
    tenant_id: Uuid,
    args: &serde_json::Value,
) -> Result<ToolCallResult, AppError> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or(AppError::InvalidInput)?;

    if query.trim().is_empty() {
        return Err(AppError::InvalidInput);
    }

    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(DEFAULT_SEARCH_LIMIT, |n| u32::try_from(n).unwrap_or(MAX_SEARCH_LIMIT))
        .clamp(1, MAX_SEARCH_LIMIT);

    let repo_id_filter: Option<Uuid> = args
        .get("repo_id")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok());

    let qdrant = state.qdrant.as_deref().ok_or(AppError::ServiceUnavailable)?;
    let ollama_url = state.config.ollama_url.as_deref().ok_or(AppError::ServiceUnavailable)?;

    if let Some(rid) = repo_id_filter {
        let owned: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM control.repos WHERE id = $1 AND tenant_id = $2 AND archived_at IS NULL"
        )
        .bind(rid)
        .bind(tenant_id)
        .fetch_optional(&state.pool)
        .await?;
        owned.ok_or(AppError::NotFound)?;
    }

    let vector = embed_query(&state.http_client, ollama_url, &state.config.embedding_model, query)
        .await?;

    let tenant = TenantId::from(tenant_id);
    let opts = SearchOptions { limit, repo_id: repo_id_filter };
    let hits = semantic_search(qdrant, &tenant, &vector, opts).await?;

    let results: Vec<serde_json::Value> = hits
        .into_iter()
        .map(|h| {
            let crate_name = h.fqn.split("::").next().unwrap_or(&h.fqn).to_owned();
            serde_json::json!({
                "fqn": h.fqn,
                "crate_name": crate_name,
                "repo_id": h.repo_id,
                "score": h.score
            })
        })
        .collect();

    let text = serde_json::to_string_pretty(&results).unwrap_or_default();
    Ok(ToolCallResult::success(text))
}

pub async fn dispatch_get_item(
    state: &AppState,
    tenant_id: Uuid,
    args: &serde_json::Value,
) -> Result<ToolCallResult, AppError> {
    let repo_id: Uuid = args
        .get("repo_id")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .ok_or(AppError::InvalidInput)?;

    let fqn = args
        .get("fqn")
        .and_then(|v| v.as_str())
        .ok_or(AppError::InvalidInput)?;

    let owned: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM control.repos WHERE id = $1 AND tenant_id = $2 AND archived_at IS NULL"
    )
    .bind(repo_id)
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await?;
    owned.ok_or(AppError::NotFound)?;

    let ctx = TenantCtx::new(TenantId::from(tenant_id));
    let symbol = items::get_by_fqn(&state.pool, &ctx, repo_id, fqn).await?;

    match symbol {
        None => Ok(ToolCallResult::success("No symbol found for the given repo_id and fqn.")),
        Some(s) => {
            let payload = serde_json::json!({
                "id": s.id,
                "fqn": s.fqn,
                "kind": s.kind,
                "repo_id": repo_id,
                "source_path": s.source_path,
                "line_start": s.line_start,
                "line_end": s.line_end,
                "source_text": s.source_text,
                "blob_ref": s.blob_ref
            });
            let text = serde_json::to_string_pretty(&payload).unwrap_or_default();
            Ok(ToolCallResult::success(text))
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
pub async fn dispatch_get_callers(
    state: &AppState,
    tenant_id: Uuid,
    args: &serde_json::Value,
) -> Result<ToolCallResult, AppError> {
    let repo_id: Uuid = args
        .get("repo_id")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .ok_or(AppError::InvalidInput)?;

    let fqn = args
        .get("fqn")
        .and_then(|v| v.as_str())
        .ok_or(AppError::InvalidInput)?;

    let depth = args
        .get("depth")
        .and_then(serde_json::Value::as_u64)
        .map_or(rb_query::DEFAULT_DEPTH, |n| n as u32)
        .clamp(1, rb_query::MAX_DEPTH);

    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(rb_query::DEFAULT_LIMIT, |n| n as usize)
        .clamp(1, rb_query::MAX_LIMIT);

    let graph = state.graph.as_deref().ok_or(AppError::ServiceUnavailable)?;

    let owned: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM control.repos WHERE id = $1 AND tenant_id = $2 AND archived_at IS NULL"
    )
    .bind(repo_id)
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await?;
    owned.ok_or(AppError::NotFound)?;

    let tenant = TenantId::from(tenant_id);
    let opts = TraversalOptions { depth, limit, offset: 0 };
    let result = fetch_callers(graph, &tenant, repo_id, fqn, opts).await?;

    let text = serde_json::to_string_pretty(&result).unwrap_or_default();
    Ok(ToolCallResult::success(text))
}

#[allow(clippy::cast_possible_truncation)]
pub async fn dispatch_get_callees(
    state: &AppState,
    tenant_id: Uuid,
    args: &serde_json::Value,
) -> Result<ToolCallResult, AppError> {
    let repo_id: Uuid = args
        .get("repo_id")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .ok_or(AppError::InvalidInput)?;

    let fqn = args
        .get("fqn")
        .and_then(|v| v.as_str())
        .ok_or(AppError::InvalidInput)?;

    let depth = args
        .get("depth")
        .and_then(serde_json::Value::as_u64)
        .map_or(rb_query::DEFAULT_DEPTH, |n| n as u32)
        .clamp(1, rb_query::MAX_DEPTH);

    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(rb_query::DEFAULT_LIMIT, |n| n as usize)
        .clamp(1, rb_query::MAX_LIMIT);

    let graph = state.graph.as_deref().ok_or(AppError::ServiceUnavailable)?;

    let owned: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM control.repos WHERE id = $1 AND tenant_id = $2 AND archived_at IS NULL"
    )
    .bind(repo_id)
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await?;
    owned.ok_or(AppError::NotFound)?;

    let tenant = TenantId::from(tenant_id);
    let opts = TraversalOptions { depth, limit, offset: 0 };
    let result = fetch_callees(graph, &tenant, repo_id, fqn, opts).await?;

    let text = serde_json::to_string_pretty(&result).unwrap_or_default();
    Ok(ToolCallResult::success(text))
}

pub async fn dispatch_get_trait_impls(
    state: &AppState,
    tenant_id: Uuid,
    args: &serde_json::Value,
) -> Result<ToolCallResult, AppError> {
    let repo_id: Uuid = args
        .get("repo_id")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .ok_or(AppError::InvalidInput)?;

    let fqn = args
        .get("fqn")
        .and_then(|v| v.as_str())
        .ok_or(AppError::InvalidInput)?;

    let graph = state.graph.as_deref().ok_or(AppError::ServiceUnavailable)?;

    let owned: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM control.repos WHERE id = $1 AND tenant_id = $2 AND archived_at IS NULL"
    )
    .bind(repo_id)
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await?;
    owned.ok_or(AppError::NotFound)?;

    let tenant = TenantId::from(tenant_id);
    let impls = fetch_trait_impls(graph, &tenant, repo_id, fqn).await?;

    let results: Vec<serde_json::Value> = impls
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "fqn": e.fqn,
                "impl_kind": e.impl_kind
            })
        })
        .collect();

    let text = serde_json::to_string_pretty(&results).unwrap_or_default();
    Ok(ToolCallResult::success(text))
}

pub async fn dispatch_run_query(
    state: &AppState,
    tenant_id: Uuid,
    args: &serde_json::Value,
) -> Result<ToolCallResult, AppError> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or(AppError::InvalidInput)?;

    if query.trim().is_empty() {
        return Err(AppError::InvalidInput);
    }

    let repo_id_filter: Option<Uuid> = args
        .get("repo_id")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok());

    if let Some(rid) = repo_id_filter {
        let owned: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM control.repos WHERE id = $1 AND tenant_id = $2 AND archived_at IS NULL"
        )
        .bind(rid)
        .bind(tenant_id)
        .fetch_optional(&state.pool)
        .await?;
        owned.ok_or(AppError::NotFound)?;
    }

    let graph = state.graph.as_deref().ok_or(AppError::ServiceUnavailable)?;

    let read_only_query = validate_read_only(query)?;

    let tenant = TenantId::from(tenant_id);
    let params = repo_id_filter
        .map(|rid| {
            let mut map = serde_json::Map::new();
            map.insert("repo_id".to_owned(), serde_json::json!(rid.to_string()));
            map
        })
        .unwrap_or_default();
    
    let results: Vec<serde_json::Value> = graph
        .execute_query(&tenant, &read_only_query, &params)
        .await
        .map_err(|_| AppError::ServiceUnavailable)?;

    let text = serde_json::to_string_pretty(&results).unwrap_or_default();
    Ok(ToolCallResult::success(text))
}

fn validate_read_only(query: &str) -> Result<String, AppError> {
    let normalized = query.to_ascii_uppercase();

    let forbidden = ["CREATE", "DELETE", "DROP", "MERGE", "SET", "REMOVE", "CALL"];
    for kw in &forbidden {
        if normalized.contains(kw) {
            return Err(AppError::InvalidInput);
        }
    }

    Ok(query.to_owned())
}

#[allow(clippy::cast_possible_truncation)]
async fn embed_query(
    http: &reqwest::Client,
    ollama_url: &str,
    model: &str,
    query: &str,
) -> Result<Vec<f32>, AppError> {
    let url = format!("{}/api/embeddings", ollama_url.trim_end_matches('/'));
    let body = serde_json::json!({ "model": model, "prompt": query });

    let resp = http
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|_| AppError::ServiceUnavailable)?;

    if !resp.status().is_success() {
        return Err(AppError::ServiceUnavailable);
    }

    let json: serde_json::Value = resp.json().await.map_err(|_| AppError::ServiceUnavailable)?;

    let embedding = json
        .get("embedding")
        .and_then(serde_json::Value::as_array)
        .ok_or(AppError::ServiceUnavailable)?;

    embedding
        .iter()
        .map(|v| v.as_f64().map(|f| f as f32).ok_or(AppError::ServiceUnavailable))
        .collect()
}
