use crate::task_mgr::{ScheduledTask, TaskContext};
use anyhow::Result;
use omnish_common::config::ConfigMap;
use std::time::Duration;
use tokio_cron_scheduler::Job;

pub struct EvictionTask {
    config: ConfigMap,
    schedule: String,
}

impl EvictionTask {
    pub fn new(config: ConfigMap) -> Self {
        let schedule = config.get_string("schedule", "");
        Self { config, schedule }
    }
}

impl ScheduledTask for EvictionTask {
    fn name(&self) -> &'static str {
        "eviction"
    }

    fn schedule(&self) -> &str {
        &self.schedule
    }

    fn enabled(&self) -> bool {
        self.config.get_bool("enabled", true)
    }

    fn defaults() -> std::collections::HashMap<String, serde_json::Value> {
        [
            ("enabled".into(), serde_json::json!(true)),
            ("schedule".into(), serde_json::json!("0 0 * * * *")),
            ("session_evict_hours".into(), serde_json::json!(48)),
        ].into()
    }

    fn create_job(&self, ctx: &TaskContext) -> Result<Job> {
        let mgr = ctx.session_mgr.clone();
        let hours = self.config.get_u64("session_evict_hours", 48);
        let max_inactive = Duration::from_secs(hours * 3600);
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
