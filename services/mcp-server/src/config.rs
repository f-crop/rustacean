use std::env;

use anyhow::{Context as _, Result, bail};

/// MCP Server configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct Config {
    pub listen_addr: String,
    pub database_url: String,
    pub service_name: String,
    pub neo4j_uri: Option<String>,
    pub neo4j_user: String,
    pub neo4j_password: Option<String>,
    pub qdrant_url: Option<String>,
    pub ollama_url: Option<String>,
    pub embedding_model: String,
}

impl Config {
    /// Load configuration from environment variables.
    ///
    /// # Errors
    ///
    /// Returns an error if required variables are missing or invalid.
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            listen_addr: env::var("RB_MCP_LISTEN_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:8080".to_owned()),
            database_url: env::var("DATABASE_URL")
                .context("DATABASE_URL must be set")?,
            service_name: env::var("RB_SERVICE_NAME")
                .unwrap_or_else(|_| "mcp-server".to_owned()),
            neo4j_uri: env::var("RB_NEO4J_URI").ok(),
            neo4j_user: env::var("RB_NEO4J_USER").unwrap_or_else(|_| "neo4j".to_owned()),
            neo4j_password: env::var("RB_NEO4J_PASSWORD").ok(),
            qdrant_url: env::var("RB_QDRANT_URL").ok(),
            ollama_url: env::var("RB_OLLAMA_URL").ok(),
            embedding_model: env::var("RB_EMBEDDING_MODEL")
                .unwrap_or_else(|_| "nomic-embed-text".to_owned()),
        })
    }

    /// Validate configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if configuration is inconsistent.
    pub fn validate(&self) -> Result<()> {
        if self.neo4j_uri.is_some() && self.neo4j_password.is_none() {
            bail!("RB_NEO4J_PASSWORD must be set when RB_NEO4J_URI is configured");
        }
        Ok(())
    }
}
