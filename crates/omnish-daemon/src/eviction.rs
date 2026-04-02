use crate::task_mgr::{ScheduledTask, TaskContext};
use anyhow::Result;
use std::time::Duration;
use tokio_cron_scheduler::Job;

pub struct EvictionTask(pub omnish_common::config::EvictionConfig);

impl ScheduledTask for EvictionTask {
    fn name(&self) -> &'static str {
        "eviction"
    }

    fn schedule(&self) -> &str {
        "0 0 * * * *"
    }

    fn enabled(&self) -> bool {
        self.0.enabled
    }

    fn create_job(&self, ctx: &TaskContext) -> Result<Job> {
        let mgr = ctx.session_mgr.clone();
        let max_inactive = Duration::from_secs(self.0.session_evict_hours * 3600);
        Ok(Job::new_async(self.schedule(), move |_uuid, _lock| {
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
}
