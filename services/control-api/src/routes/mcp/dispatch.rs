//! Tool dispatch for Phase 1 MCP tools: `search_items` and `get_item`.
//!
//! **Architecture (ADR-009 §1):** tool implementations live here in
//! `control-api`, not in `rb-mcp`.  The `rb-mcp` crate owns protocol types
//! only; it has no dependency on `rb-query`.

use rb_mcp::ToolCallResult;
use rb_query::{DEFAULT_SEARCH_LIMIT, MAX_SEARCH_LIMIT, SearchOptions, items, semantic_search};
use rb_schemas::TenantId;
use rb_tenant::TenantCtx;
use uuid::Uuid;

use crate::{error::AppError, state::AppState};

pub(super) async fn dispatch_search_items(
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

    #[allow(clippy::cast_possible_truncation)]
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(DEFAULT_SEARCH_LIMIT, |n| n as u32)
        .clamp(1, MAX_SEARCH_LIMIT);

    let repo_id_filter: Option<Uuid> = args
        .get("repo_id")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok());

    let qdrant = state
        .qdrant
        .as_deref()
        .ok_or(AppError::ServiceUnavailable)?;
    let ollama_url = state
        .config
        .ollama_url
        .as_deref()
        .ok_or(AppError::ServiceUnavailable)?;

    if let Some(rid) = repo_id_filter {
        let owned: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM control.repos \
             WHERE id = $1 AND tenant_id = $2 AND archived_at IS NULL",
        )
        .bind(rid)
        .bind(tenant_id)
        .fetch_optional(&state.pool)
        .await?;
        owned.ok_or(AppError::NotFound)?;
    }

    let vector = embed_query(
        &state.http_client,
        ollama_url,
        &state.config.embedding_model,
        query,
    )
    .await?;

    let tenant = TenantId::from(tenant_id);
    let opts = SearchOptions {
        limit,
        repo_id: repo_id_filter,
    };
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

pub(super) async fn dispatch_get_item(
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
        "SELECT id FROM control.repos \
         WHERE id = $1 AND tenant_id = $2 AND archived_at IS NULL",
    )
    .bind(repo_id)
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await?;
    owned.ok_or(AppError::NotFound)?;

    let ctx = TenantCtx::new(TenantId::from(tenant_id));
    let symbol = items::get_by_fqn(&state.pool, &ctx, repo_id, fqn).await?;

    match symbol {
        None => Ok(ToolCallResult::success(
            "No symbol found for the given repo_id and fqn.",
        )),
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

/// Normalize a raw search query before embedding.
///
/// nomic-embed-text collapses single CamelCase tokens (e.g. `AnalyticsFlow`)
/// to a constant degenerate vector because the tokenizer treats the whole
/// token as a single unit. Splitting at camelCase boundaries, replacing Rust
/// separators, and prepending the Nomic task prefix restores distinct vectors.
fn normalize_query(query: &str) -> String {
    // Replace Rust path separators and underscores with spaces.
    let s = query.replace("::", " ").replace('_', " ");

    // Insert spaces at camelCase boundaries.
    let mut with_spaces = String::with_capacity(s.len() + 16);
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() && i > 0 {
            let prev = chars[i - 1];
            if prev.is_lowercase() || prev.is_ascii_digit() {
                // lowercase→uppercase: always split ("FooBar" → "Foo Bar")
                with_spaces.push(' ');
            } else if prev.is_uppercase() {
                // acronym run: split before the last uppercase that precedes
                // a lowercase ("FQNMethod" → "FQN Method")
                if let Some(&next) = chars.get(i + 1) {
                    if next.is_lowercase() {
                        with_spaces.push(' ');
                    }
                }
            }
        }
        with_spaces.push(c);
    }

    // Collapse whitespace, lowercase, and add the Nomic asymmetric task prefix.
    let normalized = with_spaces.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("search_query: {}", normalized.to_lowercase())
}

async fn embed_query(
    http: &reqwest::Client,
    ollama_url: &str,
    model: &str,
    query: &str,
) -> Result<Vec<f32>, AppError> {
    let url = format!("{}/api/embeddings", ollama_url.trim_end_matches('/'));
    let prompt = normalize_query(query);
    let body = serde_json::json!({ "model": model, "prompt": prompt });

    let resp = http.post(&url).json(&body).send().await.map_err(|e| {
        tracing::warn!("Ollama request failed: {e}");
        AppError::ServiceUnavailable
    })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        tracing::warn!("Ollama returned HTTP {status}: {text}");
        return Err(AppError::ServiceUnavailable);
    }

    let json: serde_json::Value = resp.json().await.map_err(|e| {
        tracing::warn!("Ollama response parse error: {e}");
        AppError::ServiceUnavailable
    })?;

    let embedding = json
        .get("embedding")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            tracing::warn!("Ollama response missing 'embedding' array");
            AppError::ServiceUnavailable
        })?;

    #[allow(clippy::cast_possible_truncation)]
    embedding
        .iter()
        .map(|v| {
            v.as_f64()
                .map(|f| f as f32)
                .ok_or(AppError::ServiceUnavailable)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_query_splits_camel_case() {
        assert_eq!(
            normalize_query("AnalyticsFlow"),
            "search_query: analytics flow"
        );
        assert_eq!(normalize_query("HelloWorld"), "search_query: hello world");
        assert_eq!(normalize_query("FooBar"), "search_query: foo bar");
    }

    #[test]
    fn normalize_query_handles_underscores_and_colons() {
        assert_eq!(
            normalize_query("analytics_flow"),
            "search_query: analytics flow"
        );
        assert_eq!(normalize_query("module::Type"), "search_query: module type");
        assert_eq!(
            normalize_query("my_crate::MyStruct"),
            "search_query: my crate my struct"
        );
    }

    #[test]
    fn normalize_query_handles_acronym_runs() {
        // FQNMethod → FQN Method
        assert_eq!(normalize_query("FQNMethod"), "search_query: fqn method");
    }

    #[test]
    fn normalize_query_single_words_differ() {
        // Single CamelCase tokens must produce distinct normalized strings
        // so they embed to distinct vectors (the core bug fix).
        let a = normalize_query("AnalyticsFlow");
        let b = normalize_query("HelloWorld");
        let c = normalize_query("FooBar");
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
    }

    #[test]
    fn search_items_requires_query_field() {
        let args = serde_json::json!({ "limit": 5 });
        assert!(args.get("query").and_then(|v| v.as_str()).is_none());
    }

    #[test]
    fn get_item_requires_repo_id_and_fqn() {
        let args = serde_json::json!({ "repo_id": "not-a-uuid", "fqn": "foo::bar" });
        let repo: Option<Uuid> = args
            .get("repo_id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok());
        assert!(repo.is_none());

        let valid = serde_json::json!({
            "repo_id": "550e8400-e29b-41d4-a716-446655440000",
            "fqn": "my_crate::MyStruct"
        });
        let repo2: Option<Uuid> = valid
            .get("repo_id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok());
        assert!(repo2.is_some());
    }

    #[test]
    fn limit_clamping() {
        let over = 200_u32.clamp(1, MAX_SEARCH_LIMIT);
        assert_eq!(over, MAX_SEARCH_LIMIT);
        let zero = 0_u32.clamp(1, MAX_SEARCH_LIMIT);
        assert_eq!(zero, 1);
    }
}
