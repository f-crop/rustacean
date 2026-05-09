use std::path::PathBuf;
use std::time::Duration;

pub fn spawn_workspace_gc(workspace_base: PathBuf) {
    let ttl_days = std::env::var("RB_AGENT_WORKSPACE_TTL_DAYS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(7);
    let ttl = Duration::from_secs(ttl_days * 24 * 3600);

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(6 * 3600));
        loop {
            interval.tick().await;
            let base = workspace_base.clone();
            tokio::task::spawn_blocking(move || gc_workspaces(&base, ttl))
                .await
                .ok();
        }
    });
}

fn gc_workspaces(base: &PathBuf, ttl: Duration) {
    let now = std::time::SystemTime::now();
    let Ok(tenant_dirs) = std::fs::read_dir(base) else {
        return;
    };

    for tenant_entry in tenant_dirs.flatten() {
        let tenant_name = tenant_entry.file_name();
        let tenant_str = tenant_name.to_string_lossy();
        if tenant_str.contains('/') || tenant_str.contains("..") {
            tracing::warn!("GC: skipping suspicious tenant directory: {}", tenant_str);
            continue;
        }

        let Ok(session_dirs) = std::fs::read_dir(tenant_entry.path()) else {
            continue;
        };
        for session_entry in session_dirs.flatten() {
            let path = session_entry.path();

            let Ok(relative_path) = path.strip_prefix(base) else {
                tracing::warn!(
                    "GC: skipping path outside workspace base: {}",
                    path.display()
                );
                continue;
            };
            let components: Vec<_> = relative_path.components().collect();
            if components.len() != 2 {
                tracing::warn!("GC: skipping unexpected path structure: {}", path.display());
                continue;
            }

            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let Ok(mtime) = meta.modified() else {
                continue;
            };
            let Ok(age) = now.duration_since(mtime) else {
                continue;
            };
            if age > ttl {
                if let Err(e) = std::fs::remove_dir_all(&path) {
                    tracing::warn!("GC: failed to remove {}: {e}", path.display());
                } else {
                    tracing::info!("GC: removed expired workspace {}", path.display());
                }
            }
        }
    }
}
