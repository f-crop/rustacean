//! Host-side tool-dispatch bridge (ADR-009 §1, §6.4).
//!
//! `ControlApiToolDispatch` implements `rb_agent_runtime::ToolDispatch` for
//! the `control-api` process.  Tool callbacks route from the runtime adapter
//! into `rb-query` via this module.  `rb-agent-runtime` never imports
//! `rb-query` directly.

use async_trait::async_trait;
use rb_agent_runtime::ToolDispatch;
use rb_query::ModuleTreeCache;
use rb_storage_qdrant::TenantVectorStore;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

#[allow(dead_code)]
#[derive(Clone)]
pub struct ControlApiToolDispatch {
    pool: PgPool,
    _qdrant: Option<Arc<TenantVectorStore>>,
    _module_tree_cache: ModuleTreeCache,
}

#[allow(dead_code)]
impl ControlApiToolDispatch {
    pub fn new(
        pool: PgPool,
        qdrant: Option<Arc<TenantVectorStore>>,
        module_tree_cache: ModuleTreeCache,
    ) -> Self {
        Self {
            pool,
            _qdrant: qdrant,
            _module_tree_cache: module_tree_cache,
        }
    }
}

#[async_trait]
impl ToolDispatch for ControlApiToolDispatch {
    async fn call(
        &self,
        tenant_id: Uuid,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        match tool_name {
            "search_items" => self.search_items(tenant_id, arguments).await,
            "get_item" => self.get_item(tenant_id, arguments).await,
            "list_repos" => self.list_repos(tenant_id).await,
            "index_code_now" => self.index_code_now(tenant_id, arguments).await,
            _ => Err(format!("unknown tool: {tool_name}")),
        }
    }
}

#[allow(dead_code, clippy::unused_async)]
impl ControlApiToolDispatch {
    async fn search_items(
        &self,
        tenant_id: Uuid,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let query = args["query"]
            .as_str()
            .ok_or("search_items: 'query' argument is required")?;

        Ok(serde_json::json!({
            "results": [],
            "message": format!("search_items for query={query:?}, tenant={tenant_id}")
        }))
    }

    async fn get_item(
        &self,
        tenant_id: Uuid,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let fqn = args["fqn"]
            .as_str()
            .ok_or("get_item: 'fqn' argument is required")?;

        Ok(serde_json::json!({
            "item": null,
            "message": format!("get_item for fqn={fqn:?}, tenant={tenant_id}")
        }))
    }

    async fn list_repos(
        &self,
        tenant_id: Uuid,
    ) -> Result<serde_json::Value, String> {
        // Dynamic query — repos table columns validated at runtime.
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT id, name FROM repos WHERE tenant_id = $1 ORDER BY name LIMIT 50",
        )
        .bind(tenant_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("DB error: {e}"))?;

        let repos: Vec<serde_json::Value> = rows
            .iter()
            .map(|(id, name)| serde_json::json!({ "id": id, "name": name }))
            .collect();

        Ok(serde_json::json!({ "repos": repos }))
    }

    async fn index_code_now(
        &self,
        tenant_id: Uuid,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let repo_id = args["repo_id"]
            .as_str()
            .ok_or("index_code_now: 'repo_id' argument is required")?;
        let repo_uuid = Uuid::parse_str(repo_id).map_err(|_| "index_code_now: invalid repo_id UUID")?;

        // Verify the repo exists and belongs to the tenant
        let repo_exists: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM control.repos WHERE id = $1 AND tenant_id = $2 AND archived_at IS NULL"
        )
        .bind(repo_uuid)
        .bind(tenant_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| format!("DB error: {e}"))?;

        if repo_exists.is_none() {
            return Err(format!("index_code_now: repo {repo_id} not found or access denied"));
        }

        // Check if there's already an in-flight run
        let in_flight: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM control.ingestion_runs WHERE repo_id = $1 AND tenant_id = $2 AND status IN ('queued', 'running') LIMIT 1"
        )
        .bind(repo_uuid)
        .bind(tenant_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| format!("DB error: {e}"))?;

        if in_flight.is_some() {
            return Ok(serde_json::json!({
                "success": false,
                "message": format!("Indexing already in progress for repo {repo_id}")
            }));
        }

        // Queue the ingestion run
        let run_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO control.ingestion_runs (id, tenant_id, repo_id, status, requested_by) VALUES ($1, $2, $3, 'queued', $4)"
        )
        .bind(run_id)
        .bind(tenant_id)
        .bind(repo_uuid)
        .bind(tenant_id)  // system-initiated via tool
        .execute(&self.pool)
        .await
        .map_err(|e| format!("DB error: {e}"))?;

        tracing::info!(%run_id, %repo_uuid, %tenant_id, "index_code_now: ingestion run queued via tool");

        Ok(serde_json::json!({
            "success": true,
            "run_id": run_id,
            "repo_id": repo_uuid,
            "message": format!("Indexing queued for repo {repo_id}")
        }))
    }
}
