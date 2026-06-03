use std::time::Duration;

use crate::adapters::{AgentProcess, RuntimeAdapter};

/// Waits for a terminated child process and returns its exit code.
///
/// If `proc.child` is `None` (taken by `natural_exit_handler` before abort),
/// returns -1 immediately.  Otherwise waits up to the grace period, then
/// force-kills and returns the exit code.
pub(super) async fn wait_terminated(
    proc: &mut AgentProcess,
    adapter: &dyn RuntimeAdapter,
    session_id: &str,
) -> i32 {
    let Some(mut child) = proc.child.take() else {
        return -1;
    };
    let timeout = Duration::from_secs(super::PROCESS_TERMINATE_TIMEOUT_SECS);
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => status.code().unwrap_or(-1),
        Ok(Err(_)) => -1,
        Err(_) => {
            tracing::warn!(
                session_id = %session_id,
                "Process termination timeout, forcing SIGKILL"
            );
            let _ = adapter.terminate(proc, true).await;
            match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
                Ok(Ok(status)) => status.code().unwrap_or(-1),
                _ => -1,
            }
        }
    }
}
