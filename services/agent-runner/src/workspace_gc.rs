use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tokio::fs;
use tracing::{error, info, warn};

pub const GC_INTERVAL_MINUTES: u64 = 360;
pub const DEFAULT_WORKSPACE_TTL_DAYS: i64 = 7;

pub struct WorkspaceGc {
    workspace_base: String,
    ttl_days: i64,
    interval_minutes: u64,
}

impl WorkspaceGc {
    pub async fn new(workspace_base: &str) -> Result<Self> {
        let ttl_days = std::env::var("RB_AGENT_WORKSPACE_TTL_DAYS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_WORKSPACE_TTL_DAYS);

        let interval_minutes = std::env::var("RB_AGENT_GC_INTERVAL_MINUTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(GC_INTERVAL_MINUTES);

        fs::create_dir_all(workspace_base)
            .await
            .context("failed to create workspace base directory")?;

        Ok(Self {
            workspace_base: workspace_base.to_owned(),
            ttl_days,
            interval_minutes,
        })
    }

    #[allow(clippy::unused_async)]
    pub fn start(self: std::sync::Arc<Self>) {
        tokio::spawn(async move {
            let interval = Duration::from_secs(self.interval_minutes * 60);
            let mut ticker = tokio::time::interval(interval);

            loop {
                ticker.tick().await;
                if let Err(e) = self.run_gc().await {
                    error!(error = %e, "workspace GC failed");
                }
            }
        });
    }

    async fn run_gc(&self) -> Result<(usize, u64)> {
        let cutoff = Utc::now() - chrono::Duration::days(self.ttl_days);
        let mut cleaned = 0usize;
        let mut bytes_freed = 0u64;

        let mut tenant_entries = fs::read_dir(&self.workspace_base).await?;

        while let Some(tenant_entry) = tenant_entries.next_entry().await? {
            let tenant_path = tenant_entry.path();
            if !tenant_path.is_dir() {
                continue;
            }

            let tenant_id = tenant_entry.file_name();
            let mut session_entries = fs::read_dir(&tenant_path).await?;

            while let Some(session_entry) = session_entries.next_entry().await? {
                let session_path = session_entry.path();
                if !session_path.is_dir() {
                    continue;
                }

                let session_id = session_entry.file_name();
                let modified = match fs::metadata(&session_path).await {
                    Ok(m) => m.modified().ok(),
                    Err(_) => None,
                };

                let should_delete = if let Some(modified) = modified {
                    let modified_dt: DateTime<Utc> = modified.into();
                    modified_dt < cutoff
                } else {
                    false
                };

                if should_delete {
                    match self.calculate_dir_size(&session_path).await {
                        Ok(size) => {
                            if let Err(e) = fs::remove_dir_all(&session_path).await {
                                warn!(
                                    tenant_id = %tenant_id.to_string_lossy(),
                                    session_id = %session_id.to_string_lossy(),
                                    error = %e,
                                    "failed to remove old workspace"
                                );
                            } else {
                                info!(
                                    tenant_id = %tenant_id.to_string_lossy(),
                                    session_id = %session_id.to_string_lossy(),
                                    bytes = size,
                                    "removed old workspace"
                                );
                                cleaned += 1;
                                bytes_freed += size;
                            }
                        }
                        Err(e) => {
                            warn!(
                                tenant_id = %tenant_id.to_string_lossy(),
                                session_id = %session_id.to_string_lossy(),
                                error = %e,
                                "failed to calculate workspace size"
                            );
                        }
                    }
                }
            }

            if fs::read_dir(&tenant_path).await?.next_entry().await?.is_none() {
                if let Err(e) = fs::remove_dir(&tenant_path).await {
                    warn!(
                        tenant_id = %tenant_id.to_string_lossy(),
                        error = %e,
                        "failed to remove empty tenant directory"
                    );
                }
            }
        }

        Ok((cleaned, bytes_freed))
    }

    async fn calculate_dir_size(&self, path: &Path) -> Result<u64> {
        use std::collections::VecDeque;

        let mut total = 0u64;
        let mut dirs: VecDeque<std::path::PathBuf> = VecDeque::new();
        dirs.push_back(path.to_path_buf());

        while let Some(dir) = dirs.pop_front() {
            let mut entries = fs::read_dir(&dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let metadata = entry.metadata().await?;
                if metadata.is_file() {
                    total += metadata.len();
                } else if metadata.is_dir() {
                    dirs.push_back(entry.path());
                }
            }
        }

        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gc_interval_is_360_minutes() {
        assert_eq!(GC_INTERVAL_MINUTES, 360);
    }

    #[test]
    fn default_ttl_is_7_days() {
        assert_eq!(DEFAULT_WORKSPACE_TTL_DAYS, 7);
    }
}
