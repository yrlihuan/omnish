use crate::session_mgr::SessionManager;
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio_cron_scheduler::Job;

pub fn create_eviction_job(
    mgr: Arc<SessionManager>,
    max_inactive: Duration,
) -> Result<Job> {
    Ok(Job::new_async("0 0 * * * *", move |_uuid, _lock| {
        let mgr = mgr.clone();
        Box::pin(async move {
            tracing::debug!("task [eviction] started");
            let evicted = mgr.evict_inactive(max_inactive).await;
            if evicted > 0 {
                tracing::info!("task [eviction] evicted {} inactive sessions", evicted);
            }
            tracing::debug!("task [eviction] finished");
        })
    })?)
}
