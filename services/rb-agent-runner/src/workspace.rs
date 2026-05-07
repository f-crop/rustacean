use crate::adapter::AdapterError;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub struct WorkspaceManager {
    base_path: PathBuf,
    ttl_days: u32,
}

impl WorkspaceManager {
    pub fn new(base_path: impl AsRef<Path>, ttl_days: u32) -> Self {
        Self {
            base_path: base_path.as_ref().to_path_buf(),
            ttl_days,
        }
    }

    pub fn from_env() -> Self {
        let base_path = std::env::var("RB_AGENT_WORKSPACE_BASE")
            .unwrap_or_else(|_| "/data/workspaces".to_string());
        let ttl_days = std::env::var("RB_AGENT_WORKSPACE_TTL_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(7);
        Self::new(base_path, ttl_days)
    }

    pub async fn create_workspace(&self, tenant_id: &Uuid, session_id: &Uuid) -> Result<PathBuf, AdapterError> {
        let workspace_path = self.base_path.join(tenant_id.to_string()).join(session_id.to_string());

        tokio::fs::create_dir_all(&workspace_path)
            .await
            .map_err(|e| AdapterError::Io(e))?;

        let metadata = std::fs::metadata(&workspace_path).map_err(|e| AdapterError::Io(e))?;
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&workspace_path, permissions).map_err(|e| AdapterError::Io(e))?;

        Ok(workspace_path)
    }

    pub async fn write_mcp_config(&self, workspace_path: &Path, api_key: &str, control_api_url: &str) -> Result<(), AdapterError> {
        let mcp_config = serde_json::json!({
            "mcpServers": {
                "rust-brain": {
                    "url": format!("{}/v1/mcp", control_api_url),
                    "headers": {
                        "Authorization": format!("Bearer {}", api_key)
                    }
                }
            }
        });

        let config_path = workspace_path.join(".mcp.json");
        let config_json = serde_json::to_string_pretty(&mcp_config)
            .map_err(|e| AdapterError::JsonParse(e))?;

        tokio::fs::write(&config_path, config_json)
            .await
            .map_err(|e| AdapterError::Io(e))?;

        Ok(())
    }

    pub async fn write_opencode_config(&self, workspace_path: &Path) -> Result<(), AdapterError> {
        let config = serde_json::json!({
            "settings": {
                "output_format": "json"
            }
        });

        let config_dir = workspace_path.join(".opencode");
        tokio::fs::create_dir_all(&config_dir)
            .await
            .map_err(|e| AdapterError::Io(e))?;

        let config_path = config_dir.join("config.json");
        let config_json = serde_json::to_string_pretty(&config)
            .map_err(|e| AdapterError::JsonParse(e))?;

        tokio::fs::write(&config_path, config_json)
            .await
            .map_err(|e| AdapterError::Io(e))?;

        Ok(())
    }

    pub async fn cleanup_expired(&self) -> Result<u64, AdapterError> {
        let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(self.ttl_days as u64 * 86400);
        let mut removed_count = 0u64;

        let entries = match tokio::fs::read_dir(&self.base_path).await {
            Ok(e) => e,
            Err(_) => return Ok(0),
        };

        let mut entries = entries;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let metadata = match entry.metadata().await {
                Ok(m) => m,
                Err(_) => continue,
            };

            if let Ok(modified) = metadata.modified() {
                if modified < cutoff {
                    if let Err(e) = tokio::fs::remove_dir_all(entry.path()).await {
                        tracing::warn!("Failed to remove expired workspace {:?}: {}", entry.path(), e);
                    } else {
                        removed_count += 1;
                    }
                }
            }
        }

        Ok(removed_count)
    }
}
