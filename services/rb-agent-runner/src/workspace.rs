//! Workspace isolation for agent sessions.
//!
//! Each session gets an isolated workspace directory under
//! `/data/workspaces/<tenant_id>/<session_id>/` with mode 0700.

use std::path::PathBuf;
use std::os::unix::fs::PermissionsExt;

use tokio::fs;
use uuid::Uuid;

use crate::error::Result;

/// Default workspace base path.
const DEFAULT_WORKSPACE_BASE: &str = "/data/workspaces";

/// Default TTL for workspace garbage collection (24 hours).
const DEFAULT_WORKSPACE_TTL_SECONDS: u64 = 24 * 60 * 60;

/// Isolated workspace for an agent session.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub session_id: Uuid,
    pub tenant_id: Uuid,
    pub path: PathBuf,
}

impl Workspace {
    /// Creates or opens a workspace for the given session.
    pub async fn create(session_id: Uuid, tenant_id: Uuid) -> Result<Self> {
        let base = std::env::var("RB_WORKSPACE_BASE")
            .unwrap_or_else(|_| DEFAULT_WORKSPACE_BASE.to_string());
        
        let path = PathBuf::from(base)
            .join(tenant_id.to_string())
            .join(session_id.to_string());

        // Create directory with 0700 permissions (owner only)
        fs::create_dir_all(&path).await?;
        let permissions = std::fs::Permissions::from_mode(0o700);
        fs::set_permissions(&path, permissions).await?;

        tracing::info!(
            session_id = %session_id,
            tenant_id = %tenant_id,
            path = %path.display(),
            "created workspace"
        );

        Ok(Self {
            session_id,
            tenant_id,
            path,
        })
    }

    /// Writes a file to the workspace.
    pub async fn write_file(&self, relative_path: &str, content: &[u8]) -> Result<()> {
        let file_path = self.path.join(relative_path);
        
        // Ensure parent directory exists
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        
        fs::write(&file_path, content).await?;
        
        // Set restrictive permissions
        let permissions = std::fs::Permissions::from_mode(0o600);
        fs::set_permissions(&file_path, permissions).await?;
        
        Ok(())
    }

    /// Reads a file from the workspace.
    pub async fn read_file(&self, relative_path: &str) -> Result<Vec<u8>> {
        let file_path = self.path.join(relative_path);
        let content = fs::read(&file_path).await?;
        Ok(content)
    }

    /// Cleans up the workspace directory.
    pub async fn cleanup(&self) -> Result<()> {
        if self.path.exists() {
            fs::remove_dir_all(&self.path).await?;
            tracing::info!(
                session_id = %self.session_id,
                path = %self.path.display(),
                "cleaned up workspace"
            );
        }
        Ok(())
    }
}

/// Garbage collects old workspaces that exceed the TTL.
pub async fn garbage_collect_workspaces() -> Result<usize> {
    let base = std::env::var("RB_WORKSPACE_BASE")
        .unwrap_or_else(|_| DEFAULT_WORKSPACE_BASE.to_string());
    let ttl_seconds: u64 = std::env::var("RB_WORKSPACE_TTL_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_WORKSPACE_TTL_SECONDS);
    
    let base_path = PathBuf::from(base);
    if !base_path.exists() {
        return Ok(0);
    }

    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(ttl_seconds);
    let mut cleaned = 0usize;

    let mut entries = fs::read_dir(&base_path).await?;
    while let Some(entry) = entries.next_entry().await? {
        let tenant_path = entry.path();
        if !tenant_path.is_dir() {
            continue;
        }

        let mut session_entries = fs::read_dir(&tenant_path).await?;
        while let Some(session_entry) = session_entries.next_entry().await? {
            let session_path = session_entry.path();
            if !session_path.is_dir() {
                continue;
            }

            let metadata = fs::metadata(&session_path).await?;
            if let Ok(modified) = metadata.modified() {
                if modified < cutoff {
                    if let Err(e) = fs::remove_dir_all(&session_path).await {
                        tracing::warn!(
                            path = %session_path.display(),
                            error = %e,
                            "failed to cleanup old workspace"
                        );
                    } else {
                        cleaned += 1;
                        tracing::info!(
                            path = %session_path.display(),
                            "garbage collected workspace"
                        );
                    }
                }
            }
        }

        let is_empty = fs::read_dir(&tenant_path).await?.next_entry().await?.is_none();
        if is_empty {
            let _ = fs::remove_dir(&tenant_path).await;
        }
    }

    Ok(cleaned)
}
