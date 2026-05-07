use std::sync::Arc;

use rb_mcp::McpSessionStore;
use rb_storage_neo4j::TenantGraph;
use rb_storage_qdrant::TenantVectorStore;
use sqlx::PgPool;

use crate::config::Config;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: Arc<Config>,
    pub graph: Option<Arc<TenantGraph>>,
    pub qdrant: Option<Arc<TenantVectorStore>>,
    pub http_client: reqwest::Client,
    pub mcp_sessions: McpSessionStore,
}

impl AppState {
    #[must_use]
    pub fn new(pool: PgPool, config: Arc<Config>) -> Self {
        Self {
            pool,
            config,
            graph: None,
            qdrant: None,
            http_client: reqwest::Client::new(),
            mcp_sessions: McpSessionStore::new(),
        }
    }

    #[must_use]
    pub fn with_graph(mut self, graph: Arc<TenantGraph>) -> Self {
        self.graph = Some(graph);
        self
    }

    #[must_use]
    pub fn with_qdrant(mut self, qdrant: Arc<TenantVectorStore>) -> Self {
        self.qdrant = Some(qdrant);
        self
    }
}
