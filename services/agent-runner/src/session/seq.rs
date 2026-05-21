use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

pub(super) const GC_INTERVAL_SECS: u64 = 300;
pub(super) const MAX_AGE_SECS: u64 = 600;

pub(super) async fn next_seq(
    counters: &Mutex<HashMap<String, i64>>,
    timestamps: &Mutex<HashMap<String, Instant>>,
    session_id: &str,
) -> i64 {
    let mut c = counters.lock().await;
    let mut ts = timestamps.lock().await;
    let n = c.entry(session_id.to_owned()).or_insert(0);
    if *n >= i64::MAX - 1 {
        tracing::warn!(
            session_id = %session_id,
            "Seq counter approaching overflow, wrapping to 1"
        );
        *n = 1;
    } else {
        *n += 1;
    }
    ts.insert(session_id.to_owned(), Instant::now());
    *n
}

pub(super) fn spawn_gc(
    counters: Arc<Mutex<HashMap<String, i64>>>,
    timestamps: Arc<Mutex<HashMap<String, Instant>>>,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(GC_INTERVAL_SECS));
        let max_age = Duration::from_secs(MAX_AGE_SECS);
        loop {
            interval.tick().await;
            let now = Instant::now();
            let mut c = counters.lock().await;
            let mut ts = timestamps.lock().await;
            let before = c.len();
            ts.retain(|session_id, t| {
                let retain = now.duration_since(*t) < max_age;
                if !retain {
                    c.remove(session_id);
                }
                retain
            });
            let removed = before - c.len();
            if removed > 0 {
                tracing::debug!("GC: removed {} stale seq counter entries", removed);
            }
        }
    });
}
